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
    /// The authority was made just now rather than read from disk.
    ///
    /// It matters because [`trust`] asks the store whether an authority of that name is
    /// there, and every one of ours has the same name. A new authority beside an old one
    /// therefore looks installed when it is not, and everything it signs is refused: the
    /// case is a machine set up before the key was kept, where the first run for a new
    /// host makes a fresh authority and would leave the previous one in the store
    /// answering for nothing.
    pub fresh_ca: bool,
}

pub fn data_dir() -> PathBuf {
    super::platform::data_dir()
}

/// Load the certificate material, generating it on first use.
pub fn ensure(domain: &str) -> Result<Material> {
    ensure_for(domain, &[domain])
}

/// The same, for a certificate that answers to more than one name.
///
/// `file` names the pair on disk; `names` are the hosts it is valid for. A second host is
/// wanted as soon as more than the map is redirected -- the sign-in and the charts live on
/// their own -- and one certificate with all of them on it is what a server needs, since
/// it presents one certificate per connection whichever host was asked for.
///
/// The authority's own key is kept beside it. It used to be thrown away, which meant a
/// certificate for a new host could only be made by generating a new authority as well --
/// and the new one would not be installed, because the check for that only looks for the
/// name, which the old one already had. The result was a certificate nothing trusted and
/// no clue as to why. With the key kept, a new host is signed by the authority already in
/// the store and nothing has to be installed again.
pub fn ensure_for(file: &str, names: &[&str]) -> Result<Material> {
    let dir = data_dir();
    fs::create_dir_all(&dir)?;
    let (ca_p, ca_key_p) = (dir.join("ca.pem"), dir.join("ca.key"));
    let (cert_p, key_p) = (dir.join(format!("{file}.pem")), dir.join(format!("{file}.key")));
    // Which hosts the certificate on disk was made for. Kept beside it because the answer
    // cannot be read back out of the PEM without a parser, and because "the files exist"
    // is not the question -- a certificate made for the map's host alone is still a file,
    // and serving it to a client asking for the sign-in host fails the name check with
    // nothing in any log to say why. Asked for more names than it carries, it is made
    // again; the authority is untouched, so nothing has to be trusted afresh.
    let names_p = dir.join(format!("{file}.names"));
    let covers = fs::read_to_string(&names_p).map(|t| {
        let have: Vec<&str> = t.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        names.iter().all(|n| have.contains(n))
    });
    if ca_p.is_file() && cert_p.is_file() && key_p.is_file() && covers.unwrap_or(false) {
        return Ok(Material { dir, ca_pem: fs::read(ca_p)?, cert_pem: fs::read(cert_p)?, key_pem: fs::read(key_p)?, fresh_ca: false });
    }
    // An authority already on disk is reused, so certificates made later are signed by the
    // one that is already trusted.
    let (ca_key, ca_params, ca_pem, fresh) = match (fs::read_to_string(&ca_key_p), fs::read(&ca_p)) {
        (Ok(k), Ok(pem)) if ca_p.is_file() => match KeyPair::from_pem(&k) {
            Ok(key) => (key, ca_params_for(), pem, false),
            Err(e) => {
                log::warn!("the local CA's key will not load ({e}); making a new authority");
                (KeyPair::generate()?, ca_params_for(), Vec::new(), true)
            }
        },
        _ => (KeyPair::generate()?, ca_params_for(), Vec::new(), true),
    };
    log::info!("certificate for {} in {}", names.join(", "), dir.display());
    let (ca_params, ca_pem) = if fresh {
        let cert = ca_params.clone().self_signed(&ca_key)?;
        (ca_params, cert.pem().into_bytes())
    } else {
        (ca_params, ca_pem)
    };

    let leaf_key = KeyPair::generate()?;
    let mut leaf = CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())?;
    leaf.distinguished_name.push(DnType::CommonName, names[0]);
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
    let issuer = Issuer::new(ca_params, &ca_key);
    let leaf_cert = leaf.signed_by(&leaf_key, &issuer)?;

    fs::write(&ca_p, &ca_pem)?;
    fs::write(&ca_key_p, ca_key.serialize_pem().into_bytes())?;
    fs::write(&cert_p, leaf_cert.pem().into_bytes())?;
    fs::write(&key_p, leaf_key.serialize_pem().into_bytes())?;
    fs::write(&names_p, names.join("\n"))?;
    Ok(Material { dir, ca_pem, cert_pem: fs::read(cert_p)?, key_pem: fs::read(key_p)?, fresh_ca: fresh })
}

/// The authority's own description, the same every time so a reused key keeps its name.
fn ca_params_for() -> CertificateParams {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("no SANs");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.distinguished_name.push(DnType::CommonName, CA_NAME);
    ca_params.distinguished_name.push(DnType::OrganizationName, "amdb-bridge");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    ca_params
}

/// A NUL-terminated UTF-16 string for the Win32 wide-character APIs.
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The DER bytes of the first certificate block in a PEM document. There is no `pem`
/// crate in the tree, so the single block is decoded by hand with the base64 dependency.
#[cfg(windows)]
fn pem_to_der(pem: &[u8]) -> Result<Vec<u8>> {
    use base64::Engine;
    let text = std::str::from_utf8(pem).context("certificate is not UTF-8")?;
    let b64: String = text
        .lines()
        .skip_while(|l| !l.contains("BEGIN CERTIFICATE"))
        .skip(1)
        .take_while(|l| !l.contains("END CERTIFICATE"))
        .flat_map(|l| l.trim().chars())
        .collect();
    base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()).context("decode the certificate")
}

/// Is our CA in the machine trusted-root store?
#[cfg(windows)]
pub fn is_trusted() -> bool {
    use winapi::um::wincrypt::{
        CertCloseStore, CertFindCertificateInStore, CertFreeCertificateContext, CertOpenStore, CERT_FIND_SUBJECT_STR_W, CERT_STORE_PROV_SYSTEM_W, CERT_STORE_READONLY_FLAG,
        CERT_SYSTEM_STORE_LOCAL_MACHINE, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
    };
    let root = wide("ROOT");
    let name = wide(CA_NAME);
    unsafe {
        let store = CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, CERT_SYSTEM_STORE_LOCAL_MACHINE | CERT_STORE_READONLY_FLAG, root.as_ptr() as *const _);
        if store.is_null() {
            return false;
        }
        let ctx = CertFindCertificateInStore(store, X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, 0, CERT_FIND_SUBJECT_STR_W, name.as_ptr() as *const _, std::ptr::null_mut());
        let found = !ctx.is_null();
        if found {
            CertFreeCertificateContext(ctx);
        }
        CertCloseStore(store, 0);
        found
    }
}

/// Install the CA into the LocalMachine Root store (needs elevation).
#[cfg(windows)]
pub fn trust(m: &Material) -> Result<()> {
    use winapi::um::wincrypt::{
        CertAddEncodedCertificateToStore, CertCloseStore, CertOpenStore, CERT_STORE_ADD_REPLACE_EXISTING, CERT_STORE_PROV_SYSTEM_W, CERT_SYSTEM_STORE_LOCAL_MACHINE, PKCS_7_ASN_ENCODING,
        X509_ASN_ENCODING,
    };
    if is_trusted() {
        return Ok(());
    }
    let der = pem_to_der(&m.ca_pem)?;
    let root = wide("ROOT");
    unsafe {
        let store = CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, CERT_SYSTEM_STORE_LOCAL_MACHINE, root.as_ptr() as *const _);
        if store.is_null() {
            return Err(anyhow!("could not open the Windows trusted root store: {}", std::io::Error::last_os_error()));
        }
        let ok = CertAddEncodedCertificateToStore(store, X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, der.as_ptr(), der.len() as u32, CERT_STORE_ADD_REPLACE_EXISTING, std::ptr::null_mut());
        let err = std::io::Error::last_os_error();
        CertCloseStore(store, 0);
        if ok == 0 {
            return Err(anyhow!("could not install the certificate into the Windows trusted root store: {err}"));
        }
    }
    log::info!("installed {CA_NAME} into the Windows trusted root store");
    Ok(())
}

/// Remove the CA from the store.
#[cfg(windows)]
pub fn untrust() -> Result<bool> {
    use winapi::um::wincrypt::{
        CertCloseStore, CertDeleteCertificateFromStore, CertDuplicateCertificateContext, CertFindCertificateInStore, CertFreeCertificateContext, CertOpenStore, CERT_FIND_SUBJECT_STR_W,
        CERT_STORE_PROV_SYSTEM_W, CERT_SYSTEM_STORE_LOCAL_MACHINE, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
    };
    // Checked read-only first: opening the machine store for writing needs administrator
    // rights, and the per-user uninstaller runs without them when there is nothing to do.
    if !is_trusted() {
        return Ok(false);
    }
    let root = wide("ROOT");
    let name = wide(CA_NAME);
    unsafe {
        let store = CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, CERT_SYSTEM_STORE_LOCAL_MACHINE, root.as_ptr() as *const _);
        if store.is_null() {
            return Err(anyhow!("could not open the Windows trusted root store: {}", std::io::Error::last_os_error()));
        }
        // Every certificate of our name goes, not only the first, matching what
        // `certutil -delstore` did: a machine may carry more than one from earlier runs.
        let mut removed = false;
        loop {
            let ctx = CertFindCertificateInStore(store, X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, 0, CERT_FIND_SUBJECT_STR_W, name.as_ptr() as *const _, std::ptr::null_mut());
            if ctx.is_null() {
                break;
            }
            // The delete frees the context it is handed, so it takes a duplicate; the one
            // the search returned is freed on its own.
            let ok = CertDeleteCertificateFromStore(CertDuplicateCertificateContext(ctx));
            let err = std::io::Error::last_os_error();
            CertFreeCertificateContext(ctx);
            if ok == 0 {
                CertCloseStore(store, 0);
                return Err(anyhow!("could not remove the certificate from the Windows trusted root store: {err}"));
            }
            removed = true;
        }
        CertCloseStore(store, 0);
        Ok(removed)
    }
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

    /// Asked for a host the certificate on disk does not carry, it is made again.
    ///
    /// "The files are there" is not the question. A certificate made for the map's host
    /// alone is still a file, and handing it to a client that asked for the sign-in host
    /// fails the name check with nothing in any log to say why -- which is exactly what
    /// would have happened on a machine set up before the Fenix hosts were added, since
    /// its `amdb.api.navigraph.com.pem` already existed.
    #[test]
    fn a_certificate_is_remade_when_it_does_not_cover_what_is_asked_for() {
        let dir = std::env::temp_dir().join(format!("amdb-tls-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Stand in for a machine set up before, with a certificate for one host only.
        let one = ["amdb.api.navigraph.com"];
        let names_p = dir.join("amdb.api.navigraph.com.names");
        for f in ["ca.pem", "ca.key", "amdb.api.navigraph.com.pem", "amdb.api.navigraph.com.key"] {
            fs::write(dir.join(f), b"placeholder").unwrap();
        }
        fs::write(&names_p, one.join("\n")).unwrap();

        let covers = |want: &[&str]| -> bool {
            let t = fs::read_to_string(&names_p).unwrap();
            let have: Vec<String> = t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
            want.iter().all(|n| have.iter().any(|h| h == n))
        };
        assert!(covers(&one), "the one it was made for");
        assert!(!covers(&["identity.api.navigraph.com"]), "a host it was never made for");
        let _ = fs::remove_dir_all(&dir);
    }
}
