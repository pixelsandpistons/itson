//! TLS identity for a self-hosted voice server.
//!
//! The server makes a self-signed certificate on first start and keeps it in
//! its data directory. Clients pin the certificate's SHA-256 fingerprint,
//! which the host shares with members, so no certificate authority or domain
//! is needed.

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub const ALPN: &[u8] = b"itson-voice/0";
pub const SERVER_NAME: &str = "itson.local";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Lowercase hex SHA-256 of a DER certificate.
pub fn fingerprint(der: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, der)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Parses 64 hex digits, with or without colons.
pub fn parse_fingerprint(text: &str) -> Result<[u8; 32], BoxError> {
    let hex: String = text.trim().chars().filter(|c| *c != ':').collect();
    if hex.len() != 64 {
        return Err("a fingerprint is 64 hexadecimal digits".into());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = hex
            .get(i * 2..i * 2 + 2)
            .ok_or("fingerprint is not ASCII hex")?;
        *byte = u8::from_str_radix(pair, 16)?;
    }
    Ok(out)
}

#[derive(Debug)]
pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

/// Loads the certificate from `dir`, creating it on first start.
pub fn load_or_create(dir: &Path) -> Result<Identity, BoxError> {
    std::fs::create_dir_all(dir)?;
    let cert_path = dir.join("voice-cert.der");
    let key_path = dir.join("voice-key.der");
    if cert_path.exists() && key_path.exists() {
        return Ok(Identity {
            cert: CertificateDer::from(std::fs::read(&cert_path)?),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(std::fs::read(&key_path)?)),
        });
    }
    let made = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()])?;
    let cert = made.cert.der().to_vec();
    let key = made.signing_key.serialize_der();
    write_private(&key_path, &key)?;
    std::fs::write(&cert_path, &cert)?;
    Ok(Identity {
        cert: CertificateDer::from(cert),
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
    })
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn transport() -> Result<Arc<quinn::TransportConfig>, BoxError> {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(5)));
    t.max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
    Ok(Arc::new(t))
}

pub fn server_config(identity: Identity) -> Result<quinn::ServerConfig, BoxError> {
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![identity.cert], identity.key)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
    config.transport_config(transport()?);
    Ok(config)
}

/// Accepts only the certificate with the pinned fingerprint, then checks
/// handshake signatures normally.
#[derive(Debug)]
struct PinnedCertificate {
    sha256: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest = ring::digest::digest(&ring::digest::SHA256, end_entity.as_ref());
        if digest.as_ref() == self.sha256.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn client_config(sha256: [u8; 32]) -> Result<quinn::ClientConfig, BoxError> {
    let provider = provider();
    let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCertificate { sha256, provider }))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
    config.transport_config(transport()?);
    Ok(config)
}

/// Connects and returns the endpoint with the connection; keep both alive.
pub async fn connect(
    server: &str,
    fingerprint: &str,
) -> Result<(quinn::Endpoint, quinn::Connection), BoxError> {
    let addr = tokio::net::lookup_host(server)
        .await?
        .next()
        .ok_or("the server address did not resolve")?;
    let bind: SocketAddr = if addr.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    }
    .parse()?;
    let mut endpoint = quinn::Endpoint::client(bind)?;
    endpoint.set_default_client_config(client_config(parse_fingerprint(fingerprint)?)?);
    let connection = endpoint.connect(addr, SERVER_NAME)?.await?;
    Ok((endpoint, connection))
}

/// The server announces the speaker session on the first unidirectional stream.
pub async fn send_session(connection: &quinn::Connection, session: u32) -> Result<(), BoxError> {
    let mut stream = connection.open_uni().await?;
    stream.write_all(&session.to_be_bytes()).await?;
    stream.finish()?;
    Ok(())
}

pub async fn receive_session(connection: &quinn::Connection) -> Result<u32, BoxError> {
    let mut stream = connection.accept_uni().await?;
    let bytes = stream.read_to_end(4).await?;
    let array: [u8; 4] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "bad session announcement")?;
    Ok(u32::from_be_bytes(array))
}
