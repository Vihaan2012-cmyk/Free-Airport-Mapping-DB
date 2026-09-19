//! Local certificate authority and server certificate for the redirected Navigraph
//! host, plus installation into the Windows trusted-root store so the simulator's
//! browser engine accepts the connection.

use anyhow::{anyhow, Context, Result};
use rcgen::{BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use std::fs;
use std::path::PathBuf;
#[cfg(not(windows))]
use std::process::Command;

pub const CA_NAME: &str = "amdb-bridge local CA";

pub struct Material {
    pub dir: PathBuf,
    pub ca_pem: Vec<u8>,
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
}

pub fn data_dir() -> PathBuf {
    super::platform::data_dir()
}

/// Load the certificate material, generating it on first use.
pub fn ensure(domain: &str) -> Result<Material> {
    let dir = data_dir();
    fs::create_dir_all(&dir)?;
    let (ca_p, cert_p, key_p) = (dir.join("ca.pem"), dir.join(format!("{domain}.pem")), dir.join(format!("{domain}.key")));
    if ca_p.is_file() && cert_p.is_file() && key_p.is_file() {
        return Ok(Material { dir, ca_pem: fs::read(ca_p)?, cert_pem: fs::read(cert_p)?, key_pem: fs::read(key_p)? });
    }
    log::info!("generating local CA and certificate for {domain} in {}", dir.display());
    let ca_key = KeyPair::generate()?;
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.distinguished_name.push(DnType::CommonName, CA_NAME);
    ca_params.distinguished_name.push(DnType::OrganizationName, "amdb-bridge");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    let ca_cert = ca_params.self_signed(&ca_key)?;
    let ca_pem = ca_cert.pem().into_bytes();

    let leaf_key = KeyPair::generate()?;
    let mut leaf = CertificateParams::new(vec![domain.to_string()])?;
    leaf.distinguished_name.push(DnType::CommonName, domain);
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
    let issuer = Issuer::new(ca_params, ca_key);
    let leaf_cert = leaf.signed_by(&leaf_key, &issuer)?;
    let cert_pem = leaf_cert.pem().into_bytes();
    let key_pem = leaf_key.serialize_pem().into_bytes();

    fs::write(&ca_p, &ca_pem)?;
    fs::write(&cert_p, &cert_pem)?;
    fs::write(&key_p, &key_pem)?;
    Ok(Material { dir, ca_pem, cert_pem, key_pem })
}

/// Is our CA in the machine trusted-root store?
#[cfg(windows)]
pub fn is_trusted() -> bool {
    super::quiet_command("certutil").args(["-store", "Root"]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains(CA_NAME)).unwrap_or(false)
}

/// Install the CA into the LocalMachine Root store (needs elevation).
#[cfg(windows)]
pub fn trust(m: &Material) -> Result<()> {
    if is_trusted() {
        return Ok(());
    }
    let ca_path = m.dir.join("ca.pem");
    let out = super::quiet_command("certutil").args(["-addstore", "-f", "Root"]).arg(&ca_path).output().context("run certutil")?;
    if !out.status.success() {
        return Err(anyhow!("certutil failed: {}", String::from_utf8_lossy(&out.stdout).trim()));
    }
    log::info!("installed {CA_NAME} into the Windows trusted root store");
    Ok(())
}

/// Remove the CA from the store.
#[cfg(windows)]
pub fn untrust() -> Result<bool> {
    if !is_trusted() {
        return Ok(false);
    }
    let out = super::quiet_command("certutil").args(["-delstore", "Root", CA_NAME]).output().context("run certutil")?;
    if !out.status.success() {
        return Err(anyhow!("certutil failed: {}", String::from_utf8_lossy(&out.stdout).trim()));
    }
    Ok(true)
}

// Linux: the CA goes into the system store. Wine, and so Proton, builds its Windows
// root store from the same bundle, so MSFS under Proton trusts it too.
#[cfg(not(windows))]
const ANCHORS: [(&str, &str); 2] = [("/usr/local/share/ca-certificates/amdb-bridge.crt", "update-ca-certificates"), ("/etc/pki/ca-trust/source/anchors/amdb-bridge.pem", "update-ca-trust")];
#[cfg(not(windows))]
const BUNDLES: [&str; 2] = ["/etc/ssl/certs/ca-certificates.crt", "/etc/pki/tls/certs/ca-bundle.crt"];

#[cfg(not(windows))]
fn tool(name: &str) -> bool {
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"].iter().any(|d| std::path::Path::new(d).join(name).is_file())
}

#[cfg(not(windows))]
pub fn is_trusted() -> bool {
    let Ok(ca) = fs::read_to_string(data_dir().join("ca.pem")) else { return false };
    let Some(line) = ca.lines().nth(1) else { return false };
    BUNDLES.iter().any(|b| fs::read_to_string(b).map_or(false, |t| t.contains(line)))
}

#[cfg(not(windows))]
pub fn trust(m: &Material) -> Result<()> {
    if is_trusted() {
        return Ok(());
    }
    let ca = m.dir.join("ca.pem");
    for (anchor, update) in ANCHORS {
        if tool(update) {
            let dir = std::path::Path::new(anchor).parent().unwrap();
            fs::create_dir_all(dir).with_context(|| format!("create {} (run with sudo)", dir.display()))?;
            fs::copy(&ca, anchor).with_context(|| format!("copy the certificate to {anchor} (run with sudo)"))?;
            let out = Command::new(update).output().with_context(|| format!("run {update}"))?;
            if !out.status.success() {
                return Err(anyhow!("{update} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            log::info!("installed {CA_NAME} into the system certificate store ({update})");
            return Ok(());
        }
    }
    if tool("trust") {
        let out = Command::new("trust").args(["anchor", "--store"]).arg(&ca).output().context("run trust")?;
        if out.status.success() {
            return Ok(());
        }
        return Err(anyhow!("trust anchor failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Err(anyhow!("no certificate tool found (update-ca-certificates, update-ca-trust or trust); add {} to your system's trusted certificates by hand", ca.display()))
}

#[cfg(not(windows))]
pub fn untrust() -> Result<bool> {
    let mut removed = false;
    for (anchor, update) in ANCHORS {
        if std::path::Path::new(anchor).is_file() {
            fs::remove_file(anchor).with_context(|| format!("remove {anchor} (run with sudo)"))?;
            let _ = Command::new(update).arg("--fresh").output().or_else(|_| Command::new(update).output());
            removed = true;
        }
    }
    if !removed && tool("trust") && is_trusted() {
        let _ = Command::new("trust").args(["anchor", "--remove"]).arg(data_dir().join("ca.pem")).output();
        removed = true;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_pem_material() {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        assert!(ca.pem().starts_with("-----BEGIN CERTIFICATE-----"));
        let issuer = Issuer::new(ca_params, ca_key);
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["amdb.api.navigraph.com".to_string()]).unwrap().signed_by(&leaf_key, &issuer).unwrap();
        assert!(leaf.pem().contains("CERTIFICATE"));
        assert!(leaf_key.serialize_pem().contains("PRIVATE KEY"));
    }
}
