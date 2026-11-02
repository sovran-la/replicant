//! How a leaf checks home: the CA's chain and name, as webpki checks them,
//! and then that the certificate is home's *server* identity and not one a
//! client can also use.
//!
//! Before v0.2 one certificate carried both uses and every leaf held its key
//! (PLAN.md, "Control plane"). After the upgrade that certificate is home's
//! bootstrap, still in every early leaf's `home.pem`, still signed by the CA,
//! still naming [`super::certs::SERVER_NAME`]. Webpki alone would accept it
//! from anyone posing as home. A certificate webpki would also accept as a
//! client is therefore refused as a server: home's `server.pem` carries server
//! use only.

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::server::danger::ClientCertVerifier;
use rustls::{DigitallySignedStruct, Error, SignatureScheme};
use std::sync::Arc;

/// Refused as home: a certificate a leaf could also present.
pub const DUAL_USE: &str = "home presented a certificate that is also good for a client (the \
     pre-v0.2 shared bundle); home must present its own server.pem";

#[derive(Debug)]
pub struct HomeVerifier {
    server: Arc<WebPkiServerVerifier>,
    client: Arc<dyn ClientCertVerifier>,
}

impl HomeVerifier {
    pub fn new(
        server: Arc<WebPkiServerVerifier>,
        client: Arc<dyn ClientCertVerifier>,
    ) -> Arc<Self> {
        Arc::new(Self { server, client })
    }
}

impl ServerCertVerifier for HomeVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let ok = self.server.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        if self
            .client
            .verify_client_cert(end_entity, intermediates, now)
            .is_ok()
        {
            return Err(Error::General(DUAL_USE.into()));
        }
        Ok(ok)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.server.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.server.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.server.supported_verify_schemes()
    }
}

/// The client verifier both ends build from one CA.
pub fn client_verifier(
    roots: rustls::RootCertStore,
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> crate::Result<Arc<dyn ClientCertVerifier>> {
    WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .map_err(|e| crate::Error::Tls(format!("could not build the client verifier: {e}")))
}
