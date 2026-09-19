//! One copy of the app at a time, and the signals other copies (and the installer) send
//! it: "show your window" and "quit". Also relaunching as administrator.
//!
//! The mutex and events carry a security descriptor that lets a normal process signal
//! an elevated one. Without it, the installer (running as the user) could not ask a copy
//! elevated for the Navigraph redirect to quit.

use std::ptr;
use std::time::{Duration, Instant};
use winapi::shared::minwindef::{DWORD, FALSE};
use winapi::shared::ntdef::HANDLE;
use winapi::shared::sddl::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use winapi::shared::winerror::ERROR_ALREADY_EXISTS;
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::handleapi::CloseHandle;
use winapi::um::minwinbase::SECURITY_ATTRIBUTES;
use winapi::um::shellapi::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use winapi::um::synchapi::{CreateEventW, CreateMutexW, OpenEventW, OpenMutexW, SetEvent, WaitForSingleObject};
use winapi::um::winbase::{INFINITE, WAIT_OBJECT_0};
use winapi::um::winnt::{EVENT_MODIFY_STATE, SYNCHRONIZE};
use winapi::um::winuser::SW_SHOWNORMAL;

const MUTEX: &str = "AMDBBridge.Instance";
const SHOW: &str = "AMDBBridge.Show";
const QUIT: &str = "AMDBBridge.Quit";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Everyone may use the objects, and the low mandatory label lets processes of any
/// integrity level signal them.
fn open_security() -> SECURITY_ATTRIBUTES {
    let mut sd = ptr::null_mut();
    let sddl = wide("D:(A;;GA;;;WD)S:(ML;;NW;;;LW)");
    // The descriptor lives for the whole process, so it is never freed.
    let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1 as DWORD, &mut sd, ptr::null_mut()) };
    SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD, lpSecurityDescriptor: if ok != 0 { sd } else { ptr::null_mut() }, bInheritHandle: FALSE }
}

/// Held for as long as this copy is the running one.
pub struct Instance {
    mutex: HANDLE,
    show: HANDLE,
    quit: HANDLE,
}

impl Instance {
    /// Become the running copy, waiting up to `wait` for another to finish closing (a
    /// copy that has just relaunched itself as administrator is still on its way out).
    pub fn acquire(wait: Duration) -> Option<Instance> {
        let mut sa = open_security();
        let name = wide(MUTEX);
        let until = Instant::now() + wait;
        let mutex = loop {
            let h = unsafe { CreateMutexW(&mut sa, FALSE, name.as_ptr()) };
            let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if !h.is_null() && !existed {
                break h;
            }
            if !h.is_null() {
                unsafe { CloseHandle(h) };
            }
            if Instant::now() >= until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        let mut event = |n: &str| unsafe { CreateEventW(&mut sa, FALSE, FALSE, wide(n).as_ptr()) };
        Some(Instance { mutex, show: event(SHOW), quit: event(QUIT) })
    }

    fn fired(h: HANDLE) -> bool {
        !h.is_null() && unsafe { WaitForSingleObject(h, 0) } == WAIT_OBJECT_0
    }

    /// Another copy asked this one to show its window.
    pub fn show_requested(&self) -> bool {
        Self::fired(self.show)
    }

    /// The installer or another copy asked this one to quit.
    pub fn quit_requested(&self) -> bool {
        Self::fired(self.quit)
    }

    /// Stop being the running copy, so a relaunched one can take over.
    pub fn release(&mut self) {
        for h in [&mut self.mutex, &mut self.show, &mut self.quit] {
            if !h.is_null() {
                unsafe { CloseHandle(*h) };
                *h = ptr::null_mut();
            }
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.release();
    }
}

fn signal(name: &str) -> bool {
    let h = unsafe { OpenEventW(EVENT_MODIFY_STATE, FALSE, wide(name).as_ptr()) };
    if h.is_null() {
        return false;
    }
    let ok = unsafe { SetEvent(h) } != 0;
    unsafe { CloseHandle(h) };
    ok
}

/// Ask the running copy to bring its window forward.
pub fn ask_to_show() -> bool {
    signal(SHOW)
}

/// A copy of the app is running.
pub fn running() -> bool {
    let h = unsafe { OpenMutexW(SYNCHRONIZE, FALSE, wide(MUTEX).as_ptr()) };
    if h.is_null() {
        return false;
    }
    unsafe { CloseHandle(h) };
    true
}

/// Ask the running copy to quit and wait up to `wait` for it to go. True when no copy
/// is left running.
pub fn ask_to_quit(wait: Duration) -> bool {
    if !running() {
        return true;
    }
    signal(QUIT);
    let until = Instant::now() + wait;
    while Instant::now() < until {
        if !running() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    !running()
}

/// This process has administrator rights, from its own security token. Being able to
/// write the hosts file is not the same thing: security software or a read-only flag can
/// block that for an administrator too, and taking one for the other once made the
/// elevated copy relaunch itself without end.
pub fn is_elevated() -> bool {
    use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
    use winapi::um::securitybaseapi::GetTokenInformation;
    use winapi::um::winnt::{TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
        let mut len = 0;
        let ok = GetTokenInformation(token, TokenElevation, &mut elevation as *mut _ as _, std::mem::size_of::<TOKEN_ELEVATION>() as DWORD, &mut len);
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Start this program again as administrator with `args` (Windows asks the user first).
/// With `wait`, returns once that copy has exited. False when the user declined.
pub fn run_elevated(args: &str, wait: bool) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let file = wide(&exe.display().to_string());
    let verb = wide("runas");
    let params = wide(args);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as DWORD;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return false;
    }
    if !info.hProcess.is_null() {
        if wait {
            unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
        }
        unsafe { CloseHandle(info.hProcess) };
    }
    true
}
