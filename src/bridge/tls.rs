//! Local certificate authority and server certificate for the redirected Navigraph
//! host, plus installation into the Windows trusted-root store so the simulator's
//! browser engine accepts the connection.

use anyhow::{anyhow, Context, Result};
use rcgen::{BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use std::fs;
use std::path::PathBuf;

pub const CA_NAME: &str = "amdb-bridge local CA";

pub struct Material {
    pub dir: PathBuf,
    pub ca_pem: Vec<u8>,
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("amdb-bridge")
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
pub fn is_trusted() -> bool {
    super::quiet_command("certutil").args(["-store", "Root"]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains(CA_NAME)).unwrap_or(false)
}

/// Install the CA into the LocalMachine Root store (needs elevation).
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
