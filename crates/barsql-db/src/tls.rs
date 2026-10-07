use std::sync::{Arc, Once, OnceLock};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};

// libpq's sslmode names. MySQL only distinguishes require and verify-full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl TlsMode {
    pub fn postgres(ssl_mode: &str) -> Result<Self, String> {
        Ok(match ssl_mode {
            "" | "disable" => Self::Disable,
            "allow" | "prefer" => Self::Prefer,
            "require" => Self::Require,
            "verify-ca" => Self::VerifyCa,
            "verify-full" => Self::VerifyFull,
            other => return Err(format!("sslmode is invalid: {other}")),
        })
    }

    pub fn mysql(ssl_mode: &str) -> Self {
        match ssl_mode {
            "require" => Self::Require,
            "verify-full" => Self::VerifyFull,
            _ => Self::Disable,
        }
    }

    pub fn client_config(self) -> Option<ClientConfig> {
        let verifier: Arc<dyn ServerCertVerifier> = match self {
            Self::Disable => return None,
            Self::Prefer | Self::Require => Arc::new(AcceptAnyCert(provider())),
            Self::VerifyCa => Arc::new(ChainOnly(webpki_verifier()?)),
            Self::VerifyFull => webpki_verifier()?,
        };
        let config = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .ok()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Some(config)
    }
}

// For the HTTP clients, which only speak HTTP/1.1.
pub(crate) fn http_client_config(mode: TlsMode) -> Option<Arc<ClientConfig>> {
    let mut config = mode.client_config()?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Some(Arc::new(config))
}

// Makes ring the process-wide rustls provider. Some dependencies build their TLS config from the process default,
// and that panics once a second provider (aws-lc-rs, which tiberius pulls in) is compiled in. Safe to call often.
pub fn install_default_provider() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider())).clone()
}

fn webpki_verifier() -> Option<Arc<WebPkiServerVerifier>> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider()).build().ok()
}

fn algorithms(provider: &CryptoProvider) -> WebPkiSupportedAlgorithms {
    provider.signature_verification_algorithms
}

// sslmode=require encrypts but doesn't check the certificate.
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &algorithms(&self.0))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &algorithms(&self.0))
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        algorithms(&self.0).supported_schemes()
    }
}

// sslmode=verify-ca checks the chain but not the host name.
#[derive(Debug)]
struct ChainOnly(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for ChainOnly {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        match self.0.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) {
            Err(Error::InvalidCertificate(CertificateError::NotValidForName))
            | Err(Error::InvalidCertificate(CertificateError::NotValidForNameContext { .. })) => {
                Ok(ServerCertVerified::assertion())
            }
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modes() {
        assert_eq!(TlsMode::postgres("").unwrap(), TlsMode::Disable);
        assert_eq!(TlsMode::postgres("verify-ca").unwrap(), TlsMode::VerifyCa);
        assert!(TlsMode::postgres("bogus").is_err());
        assert_eq!(TlsMode::mysql("verify-ca"), TlsMode::Disable);
        assert_eq!(TlsMode::mysql("require"), TlsMode::Require);
        assert_eq!(TlsMode::mysql("verify-full"), TlsMode::VerifyFull);
        assert_eq!(TlsMode::mysql("disable"), TlsMode::Disable);
        assert!(TlsMode::Disable.client_config().is_none());
        assert!(TlsMode::Require.client_config().is_some());
    }
}
