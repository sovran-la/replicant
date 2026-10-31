//! Home's CA and the one identity it signs. The sovran.la/agents pattern
//! (`src/remote/certs.rs`, itself lifted from nemo), copied rather than
//! depended on: same crypto (`ring`), same validity window and backdate, same
//! bundle order (leaf, key, CA), same rule that an existing bundle is never
//! rotated.
//!
//! | file | contents | leaves home? |
//! |---|---|---|
//! | `ca.key` | home's CA private key | never |
//! | `ca.pem` | home's CA certificate | inside the bundle |
//! | `credentials.pem` | leaf certificate, its key, the CA | yes: copied to each leaf as `home.pem` |
//!
//! # The trust domain is home
//!
//! A leaf dials home with home's bundle, and home accepts only certificates its
//! own CA signed. Possession of the bundle is the right to talk to home's
//! daemon. The daemon never carries file data and every request is bounded by
//! the caller's subscriptions and cap, so what that right buys is narrow; but
//! the leaf names itself in each request, so the cap is a guard against
//! mistakes, not against a hostile leaf. PLAN.md's server-side ssh confinement
//! is the hard boundary.

use crate::timefmt::civil_from_days;
use crate::{Error, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const FILE_MODE: u32 = 0o600;
pub const VALIDITY_DAYS: i64 = 3650;
/// Backdated so a peer whose clock is a little behind still accepts it.
pub const BACKDATE_DAYS: i64 = 1;
/// The name every client verifies, whatever address it dialled. Hosts are
/// addresses; identity is the CA.
pub const SERVER_NAME: &str = "replicant.sovran.internal";
const ALT_NAMES: [&str; 4] = [SERVER_NAME, "localhost", "127.0.0.1", "::1"];

/// Make sure home has an identity; generate one on first start. Returns
/// whether anything was generated.
pub fn ensure(ca_key: &Path, ca_cert: &Path, credentials: &Path) -> Result<bool> {
    if credentials.exists() {
        read_pem(credentials)?;
        return Ok(false);
    }
    let (key, ca_pem) = if ca_key.exists() && ca_cert.exists() {
        let k = KeyPair::from_pem(&read_pem(ca_key)?)
            .map_err(|e| Error::Tls(format!("{} is not a usable key: {e}", ca_key.display())))?;
        (k, read_pem(ca_cert)?)
    } else {
        let k = KeyPair::generate().map_err(generating("the CA key"))?;
        let cert = ca_params()?
            .self_signed(&k)
            .map_err(generating("the CA certificate"))?;
        write_secret(ca_key, &k.serialize_pem())?;
        write_secret(ca_cert, &cert.pem())?;
        (k, cert.pem())
    };
    let issuer = Issuer::new(ca_params()?, key);
    let leaf_key = KeyPair::generate().map_err(generating("the identity key"))?;
    let leaf = identity_params()?
        .signed_by(&leaf_key, &issuer)
        .map_err(generating("the identity certificate"))?;
    let bundle = [leaf.pem(), leaf_key.serialize_pem(), ca_pem]
        .iter()
        .map(|b| format!("{}\n", b.trim()))
        .collect::<String>();
    write_secret(credentials, &bundle)?;
    Ok(true)
}

pub fn read_pem(path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::Tls(if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "no credentials at {}. On home run `replicant config export-creds` and save \
                 the output there",
                path.display()
            )
        } else {
            format!("could not read {}: {e}", path.display())
        })
    })?;
    if !text.contains("-----BEGIN") {
        return Err(Error::Tls(format!(
            "{} contains no PEM blocks",
            path.display()
        )));
    }
    Ok(text)
}

pub fn write_secret(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(FILE_MODE))?;
    Ok(())
}

fn generating(what: &'static str) -> impl Fn(rcgen::Error) -> Error {
    move |e| Error::Tls(format!("could not generate {what}: {e}"))
}

fn base(cn: &str, alt: Vec<String>) -> Result<CertificateParams> {
    let mut p = CertificateParams::new(alt).map_err(|e| Error::Tls(e.to_string()))?;
    let today = crate::now_secs() as i64 / 86_400;
    let (y0, m0, d0) = civil_from_days(today - BACKDATE_DAYS);
    let (y1, m1, d1) = civil_from_days(today - BACKDATE_DAYS + VALIDITY_DAYS);
    p.not_before = rcgen::date_time_ymd(y0, m0 as u8, d0 as u8);
    p.not_after = rcgen::date_time_ymd(y1, m1 as u8, d1 as u8);
    p.distinguished_name.push(DnType::CommonName, cn);
    Ok(p)
}

fn ca_params() -> Result<CertificateParams> {
    let mut p = base("replicant home CA", Vec::new())?;
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    Ok(p)
}

fn identity_params() -> Result<CertificateParams> {
    let mut alt: Vec<String> = ALT_NAMES.iter().map(|s| s.to_string()).collect();
    if let Some(h) = crate::config::hostname() {
        alt.push(h);
    }
    let mut p = base("replicant daemon", alt)?;
    p.is_ca = IsCa::ExplicitNoCa;
    p.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    p.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    p.use_authority_key_identifier_extension = true;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_generates_once_and_never_rotates() {
        let tmp = tempfile::tempdir().unwrap();
        let (k, c, b) = (
            tmp.path().join("ca.key"),
            tmp.path().join("ca.pem"),
            tmp.path().join("credentials.pem"),
        );
        assert!(ensure(&k, &c, &b).unwrap());
        let first = std::fs::read_to_string(&b).unwrap();
        assert_eq!(first.matches("-----BEGIN CERTIFICATE-----").count(), 2);
        assert_eq!(
            first.matches("PRIVATE KEY-----").count(),
            2,
            "BEGIN and END of one key"
        );
        assert!(!ensure(&k, &c, &b).unwrap());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), first);
        for p in [&k, &c, &b] {
            let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, FILE_MODE, "{}", p.display());
        }
    }

    #[test]
    fn deleted_bundle_is_reissued_from_the_same_ca() {
        let tmp = tempfile::tempdir().unwrap();
        let (k, c, b) = (
            tmp.path().join("ca.key"),
            tmp.path().join("ca.pem"),
            tmp.path().join("credentials.pem"),
        );
        ensure(&k, &c, &b).unwrap();
        let ca = std::fs::read_to_string(&c).unwrap();
        std::fs::remove_file(&b).unwrap();
        ensure(&k, &c, &b).unwrap();
        assert!(std::fs::read_to_string(&b).unwrap().contains(ca.trim()));
    }

    #[test]
    fn missing_and_empty_bundles_are_explained() {
        let tmp = tempfile::tempdir().unwrap();
        let err = read_pem(&tmp.path().join("home.pem"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("export-creds"), "{err}");
        std::fs::write(tmp.path().join("e.pem"), "").unwrap();
        assert!(
            read_pem(&tmp.path().join("e.pem"))
                .unwrap_err()
                .to_string()
                .contains("no PEM")
        );
    }
}
