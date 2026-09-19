//! Windows hosts-file redirect for the Navigraph AMDB host, added while the bridge
//! runs and removed when it stops. Every line we write carries a marker so cleanup
//! never touches anything else.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub const MARKER: &str = "# amdb-bridge";

pub fn hosts_path() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join("drivers").join("etc").join("hosts")
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
    let mut out: String = text.lines().filter(|l| !l.contains(MARKER)).map(|l| format!("{l}\r\n")).collect();
    while out.ends_with("\r\n\r\n") {
        out.pop();
        out.pop();
    }
    out
}

/// Point `domain` at 127.0.0.1 (replacing any earlier entry of ours).
pub fn install(domain: &str) -> Result<()> {
    let mut text = strip_ours(&read()?);
    if !text.is_empty() && !text.ends_with("\r\n") {
        text.push_str("\r\n");
    }
    text.push_str(&format!("127.0.0.1 {domain} {MARKER}\r\n"));
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

fn flush_dns() {
    let _ = super::quiet_command("ipconfig").arg("/flushdns").output();
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
