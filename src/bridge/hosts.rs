//! Windows hosts-file redirect for the Navigraph AMDB host, added while the bridge
//! runs and removed when it stops. Every line we write carries a marker so cleanup
//! never touches anything else.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub const MARKER: &str = "# amdb-bridge";

pub fn hosts_path() -> PathBuf {
    if !cfg!(windows) {
        // Wine and Proton resolve names through the host, so this also covers MSFS.
        return PathBuf::from("/etc/hosts");
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join("drivers").join("etc").join("hosts")
}

/// The line ending the hosts file already uses.
fn eol(text: &str) -> &'static str {
    if text.contains("\r\n") || (text.is_empty() && cfg!(windows)) {
        "\r\n"
    } else {
        "\n"
    }
}

fn read() -> Result<String> {
    let p = hosts_path();
    fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))
}

fn write(text: &str) -> Result<()> {
    let p = hosts_path();
    if fs::write(&p, text).is_ok() {
        return Ok(());
    }
    // Some tools mark the hosts file read-only; an administrator may clear that.
    if let Ok(meta) = fs::metadata(&p) {
        let mut perms = meta.permissions();
        if perms.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = fs::set_permissions(&p, perms);
        }
    }
    fs::write(&p, text).with_context(|| format!("write {} (needs administrator rights; security software may also be protecting it)", p.display()))
}

/// True when we can write the hosts file (i.e. the process is elevated).
pub fn writable() -> bool {
    fs::OpenOptions::new().append(true).open(hosts_path()).is_ok()
}

fn strip_ours(text: &str) -> String {
    let nl = eol(text);
    let mut out: String = text.lines().filter(|l| !l.contains(MARKER)).map(|l| format!("{l}{nl}")).collect();
    let double = format!("{nl}{nl}");
    while out.ends_with(&double) {
        out.truncate(out.len() - nl.len());
    }
    out
}

/// Point `domain` at 127.0.0.1 (replacing any earlier entry of ours).
pub fn install(domain: &str) -> Result<()> {
    install_all(&[domain])
}

/// The same for several hosts at once, written as one block.
///
/// The map lives on one host; the Fenix A320's flight bag wants three more -- the
/// sign-in, the charts API and the chart cycle -- and they have to go in together or the
/// next `install` strips the ones before it, since cleanup keys on the marker rather than
/// on the name.
pub fn install_all(domains: &[&str]) -> Result<()> {
    let original = read()?;
    let nl = eol(&original);
    let mut text = strip_ours(&original);
    if !text.is_empty() && !text.ends_with(nl) {
        text.push_str(nl);
    }
    for domain in domains {
        text.push_str(&format!("127.0.0.1 {domain} {MARKER}{nl}"));
    }
    write(&text)?;
    flush_dns();
    Ok(())
}

/// Remove every line we added.
pub fn remove() -> Result<bool> {
    let text = read()?;
    if !text.contains(MARKER) {
        return Ok(false);
    }
    write(&strip_ours(&text))?;
    flush_dns();
    Ok(true)
}

pub fn is_installed(domain: &str) -> bool {
    read().map(|t| t.lines().any(|l| l.contains(MARKER) && l.contains(domain))).unwrap_or(false)
}

/// Clear the DNS resolver cache so the new hosts-file entries take effect at once.
///
/// `DnsFlushResolverCache` is what `ipconfig /flushdns` calls; winapi does not declare
/// it, so it is loaded from dnsapi.dll by name. Best effort, exactly as before: if the
/// export is missing, nothing happens and nothing fails.
#[cfg(windows)]
fn flush_dns() {
    use winapi::um::libloaderapi::{FreeLibrary, GetProcAddress, LoadLibraryW};
    let dll: Vec<u16> = "dnsapi.dll".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let lib = LoadLibraryW(dll.as_ptr());
        if lib.is_null() {
            return;
        }
        let proc = GetProcAddress(lib, b"DnsFlushResolverCache\0".as_ptr() as *const _);
        if !proc.is_null() {
            let flush: extern "system" fn() -> i32 = std::mem::transmute(proc);
            flush();
        }
        FreeLibrary(lib);
    }
}

#[cfg(not(windows))]
fn flush_dns() {
    let _ = std::process::Command::new("resolvectl").arg("flush-caches").output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_marked_lines() {
        let t = "127.0.0.1 localhost\r\n127.0.0.1 amdb.api.navigraph.com # amdb-bridge\r\n::1 localhost\r\n";
        let s = strip_ours(t);
        assert_eq!(s, "127.0.0.1 localhost\r\n::1 localhost\r\n");
        assert_eq!(strip_ours("a\r\nb # amdb-bridge\r\n"), "a\r\n");
    }
}
