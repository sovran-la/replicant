//! One bundle in, a rustls server and client configuration out. The agents
//! crate's `remote/tls.rs`, trimmed.
//!
//! - The **server** presents the leaf certificate and demands a client
//!   certificate signed by the bundle's CA. No `allow_unauthenticated`: a peer
//!   without one is refused in the handshake and never reaches a handler.
//! - The **client** presents the same leaf and trusts only that CA. No webpki
//!   roots: trusting them would let any public CA impersonate home.
//!
//! The bundle is leaf, key, CA, and the parser checks that shape: exactly two
//! certificates (the last is the CA) and one key, or a refusal naming what is
//! wrong. If the generator ever emits a longer chain, both halves change.

use super::certs;
use crate::{Error, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Identity {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
    pub ca: CertificateDer<'static>,
    pub source: PathBuf,
}

/// Hand-written so a private key can never reach `{:?}` in a panic message.
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("chain", &self.chain.len())
            .field("key", &"<redacted>")
            .field("source", &self.source)
            .finish()
    }
}

/// Every rustls config in this crate uses ring, explicitly, so nothing depends
/// on a process-wide default provider being installed.
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl Identity {
    pub fn load(path: &Path) -> Result<Self> {
        parse(&certs::read_pem(path)?, path)
    }

    fn roots(&self) -> Result<RootCertStore> {
        let mut roots = RootCertStore::empty();
        roots
            .add(self.ca.clone())
            .map_err(|e| Error::Tls(format!("the CA is not usable as a trust root: {e}")))?;
        Ok(roots)
    }

    pub fn server_config(&self) -> Result<Arc<ServerConfig>> {
        let verifier =
            WebPkiClientVerifier::builder_with_provider(Arc::new(self.roots()?), provider())
                .build()
                .map_err(|e| Error::Tls(format!("could not build the client verifier: {e}")))?;
        let cfg = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Tls(e.to_string()))?
            .with_client_cert_verifier(verifier)
            .with_single_cert(self.chain.clone(), self.key.clone_key())
            .map_err(|e| Error::Tls(format!("rustls refused the identity: {e}")))?;
        Ok(Arc::new(cfg))
    }

    pub fn client_config(&self) -> Result<Arc<ClientConfig>> {
        let cfg = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Tls(e.to_string()))?
            .with_root_certificates(self.roots()?)
            .with_client_auth_cert(self.chain.clone(), self.key.clone_key())
            .map_err(|e| Error::Tls(format!("rustls refused the identity: {e}")))?;
        Ok(Arc::new(cfg))
    }
}

pub fn parse(pem: &str, path: &Path) -> Result<Identity> {
    let refuse =
        |what: String| Error::Tls(format!("{} is not a usable bundle: {what}", path.display()));
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| refuse(format!("malformed certificate ({e})")))?;
    let key = rustls_pemfile::private_key(&mut pem.as_bytes())
        .map_err(|e| refuse(format!("malformed private key ({e})")))?
        .ok_or_else(|| refuse("it holds no private key".into()))?;
    if certs.len() != 2 {
        return Err(refuse(format!(
            "it holds {} certificates; a bundle is exactly the identity then its CA",
            certs.len()
        )));
    }
    let (ca, chain) = certs.split_last().expect("two certificates");
    Ok(Identity {
        chain: chain.to_vec(),
        key,
        ca: ca.clone(),
        source: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(dir: &Path) -> PathBuf {
        let b = dir.join("credentials.pem");
        certs::ensure(&dir.join("ca.key"), &dir.join("ca.pem"), &b).unwrap();
        b
    }

    #[test]
    fn generated_bundle_parses_into_both_configs() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Identity::load(&bundle(tmp.path())).unwrap();
        assert_eq!(id.chain.len(), 1);
        id.server_config().unwrap();
        id.client_config().unwrap();
        assert!(!format!("{id:?}").contains("PRIVATE"), "key redacted");
    }

    #[test]
    fn wrong_shapes_are_refused_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let text = std::fs::read_to_string(bundle(tmp.path())).unwrap();
        let p = Path::new("x.pem");
        let (k0, k1) = (
            text.find("-----BEGIN PRIVATE KEY").unwrap(),
            text.find("-----END PRIVATE KEY-----\n").unwrap(),
        );
        let certs_only = format!(
            "{}{}",
            &text[..k0],
            &text[k1 + "-----END PRIVATE KEY-----\n".len()..]
        );
        assert!(
            parse(&certs_only, p)
                .unwrap_err()
                .to_string()
                .contains("no private key")
        );
        // Drop the leaf certificate: key + CA is one certificate short.
        let one_cert = &text[text.find("-----BEGIN PRIVATE KEY").unwrap()..];
        let err = parse(one_cert, p).unwrap_err().to_string();
        assert!(err.contains("1 certificates"), "{err}");
    }
}
