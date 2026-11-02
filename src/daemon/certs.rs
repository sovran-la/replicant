//! Home's CA and the identities it signs. The sovran.la/agents pattern
//! (`src/remote/certs.rs`, itself lifted from nemo), copied rather than
//! depended on: same crypto (`ring`), same validity window and backdate, same
//! bundle order (leaf, key, CA), same rule that an existing bundle is never
//! rotated.
//!
//! | file | contents | leaves home? |
//! |---|---|---|
//! | `ca.key` | home's CA private key | never |
//! | `ca.pem` | home's CA certificate | inside every bundle |
//! | `server.pem` | home's server certificate (server use only), key, CA | never |
//! | `credentials.pem` | the bootstrap client certificate, key, CA | yes: `config export-creds`, saved as each new leaf's `home.pem` |
//! | `state/issued/<host>.pem` | one host's client certificate, key, CA | once: the host collects it, then home deletes it |
//!
//! # The trust domain is home
//!
//! Home accepts only certificates its own CA signed, and then asks which one
//! (PLAN.md, "Control plane"). The bootstrap bundle is good for asking to be
//! enrolled and for collecting the certificate `approve` issues; a host's
//! own certificate is pinned by fingerprint in fleet.toml and *is* that host,
//! whatever the request says. Client certificates carry client use only, and
//! home's server certificate server use only, so no leaf can stand in for home.

use crate::timefmt::civil_from_days;
use crate::{Error, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const FILE_MODE: u32 = 0o600;
pub const VALIDITY_DAYS: i64 = 3650;
/// Backdated so a peer whose clock is a little behind still accepts it.
pub const BACKDATE_DAYS: i64 = 1;
/// The name every client verifies, whatever address it dialled. Hosts are
/// addresses; identity is the CA.
pub const SERVER_NAME: &str = "replicant.sovran.internal";
const ALT_NAMES: [&str; 4] = [SERVER_NAME, "localhost", "127.0.0.1", "::1"];
/// How a pinned fingerprint is written in fleet.toml.
pub const FINGERPRINT_PREFIX: &str = "sha256:";

/// Home's certificate files.
#[derive(Debug, Clone)]
pub struct Paths {
    pub ca_key: PathBuf,
    pub ca_cert: PathBuf,
    pub server: PathBuf,
    pub bootstrap: PathBuf,
}

impl Paths {
    pub fn of(dirs: &crate::config::Dirs) -> Self {
        Self {
            ca_key: dirs.ca_key(),
            ca_cert: dirs.ca_cert(),
            server: dirs.server_identity(),
            bootstrap: dirs.credentials(),
        }
    }
}

/// A freshly minted client identity: the bundle and its pin.
pub struct Issued {
    pub bundle: String,
    pub fingerprint: String,
}

/// `sha256:<hex>` of a certificate's DER: what fleet.toml pins.
pub fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let mut s = String::with_capacity(FINGERPRINT_PREFIX.len() + 64);
    s.push_str(FINGERPRINT_PREFIX);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Make sure home has its CA, server identity and bootstrap bundle; generate
/// whatever is missing. Returns whether anything was generated.
///
/// A missing CA is created only on a first start: if a bundle exists without
/// it, a new CA would sign certificates nobody trusts, so that is an error.
pub fn ensure(p: &Paths) -> Result<bool> {
    let have = |f: &PathBuf| f.exists();
    if have(&p.server) && have(&p.bootstrap) {
        read_pem(&p.server)?;
        read_pem(&p.bootstrap)?;
        return Ok(false);
    }
    let (issuer, ca_pem) = ca(p, !have(&p.server) && !have(&p.bootstrap))?;
    if !have(&p.server) {
        let (bundle, _) = mint(&issuer, &ca_pem, server_params()?)?;
        write_secret(&p.server, &bundle)?;
    }
    if !have(&p.bootstrap) {
        let (bundle, _) = mint(&issuer, &ca_pem, client_params("replicant bootstrap")?)?;
        write_secret(&p.bootstrap, &bundle)?;
    }
    Ok(true)
}

/// Mint `host`'s client certificate from home's CA.
pub fn issue(p: &Paths, host: &str) -> Result<Issued> {
    let (issuer, ca_pem) = ca(p, false)?;
    let (bundle, fingerprint) = mint(
        &issuer,
        &ca_pem,
        client_params(&format!("replicant host {host}"))?,
    )?;
    Ok(Issued {
        bundle,
        fingerprint,
    })
}

/// The fingerprint of the bootstrap bundle's certificate, or `None` when
/// there is no bundle.
pub fn bootstrap_fingerprint(bootstrap: &Path) -> Result<Option<String>> {
    if !bootstrap.exists() {
        return Ok(None);
    }
    let id = super::tls::Identity::load(bootstrap)?;
    Ok(Some(id.fingerprint()))
}

/// Load the CA, or create it when `create` (a first start).
fn ca(p: &Paths, create: bool) -> Result<(Issuer<'static, KeyPair>, String)> {
    if p.ca_key.exists() && p.ca_cert.exists() {
        let k = KeyPair::from_pem(&read_pem(&p.ca_key)?)
            .map_err(|e| Error::Tls(format!("{} is not a usable key: {e}", p.ca_key.display())))?;
        return Ok((Issuer::new(ca_params()?, k), read_pem(&p.ca_cert)?));
    }
    if !create {
        return Err(Error::Tls(format!(
            "home's CA ({}, {}) is missing but certificates it signed are not. Restore it, or \
             delete {} and {} to start a new CA (every leaf then re-enrolls)",
            p.ca_key.display(),
            p.ca_cert.display(),
            p.server.display(),
            p.bootstrap.display()
        )));
    }
    let k = KeyPair::generate().map_err(generating("the CA key"))?;
    let cert = ca_params()?
        .self_signed(&k)
        .map_err(generating("the CA certificate"))?;
    write_secret(&p.ca_key, &k.serialize_pem())?;
    write_secret(&p.ca_cert, &cert.pem())?;
    Ok((Issuer::new(ca_params()?, k), cert.pem()))
}

/// Sign `params` with a fresh key: the bundle (leaf, key, CA) and its pin.
fn mint(
    issuer: &Issuer<'static, KeyPair>,
    ca_pem: &str,
    params: CertificateParams,
) -> Result<(String, String)> {
    let key = KeyPair::generate().map_err(generating("an identity key"))?;
    let leaf = params
        .signed_by(&key, issuer)
        .map_err(generating("an identity certificate"))?;
    let bundle = [leaf.pem(), key.serialize_pem(), ca_pem.to_string()]
        .iter()
        .map(|b| format!("{}\n", b.trim()))
        .collect::<String>();
    Ok((bundle, fingerprint(leaf.der())))
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

/// [`write_secret`] by way of a private sibling and a rename, so a crash
/// leaves the old file or the new one, never half of a key.
pub fn write_secret_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    write_secret(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
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

fn leaf_params(
    cn: &str,
    alt: Vec<String>,
    eku: Vec<ExtendedKeyUsagePurpose>,
) -> Result<CertificateParams> {
    let mut p = base(cn, alt)?;
    p.is_ca = IsCa::ExplicitNoCa;
    p.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    p.extended_key_usages = eku;
    p.use_authority_key_identifier_extension = true;
    Ok(p)
}

/// Home's server identity: server use only, so it cannot dial home as a leaf.
fn server_params() -> Result<CertificateParams> {
    let mut alt: Vec<String> = ALT_NAMES.iter().map(|s| s.to_string()).collect();
    if let Some(h) = crate::config::hostname() {
        alt.push(h);
    }
    leaf_params(
        "replicant home",
        alt,
        vec![ExtendedKeyUsagePurpose::ServerAuth],
    )
}

/// A client identity: client use only, so it cannot stand in for home.
fn client_params(cn: &str) -> Result<CertificateParams> {
    leaf_params(cn, Vec::new(), vec![ExtendedKeyUsagePurpose::ClientAuth])
}

/// The pre-v0.2 shared identity, both uses in one certificate, for tests of
/// what a leaf does when home presents one.
#[cfg(test)]
pub fn legacy_bundle(p: &Paths) -> Result<String> {
    let (issuer, ca_pem) = ca(p, false)?;
    let alt: Vec<String> = ALT_NAMES.iter().map(|s| s.to_string()).collect();
    let params = leaf_params(
        "replicant daemon",
        alt,
        vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ],
    )?;
    Ok(mint(&issuer, &ca_pem, params)?.0)
}

#[cfg(test)]
#[path = "certs_tests.rs"]
mod tests;
