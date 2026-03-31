use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use quinn::{ClientConfig, Endpoint, RecvStream, SendStream, ServerConfig};
use tracing::info;

use crate::protocol::ControlMessage;

/// Self-signed TLS config for the control channel.
/// In production this would use proper certificates, but for P2P file transfer
/// between trusted parties, self-signed is appropriate.
fn make_server_config() -> Result<ServerConfig> {
    let cert = rcgen::generate_simple_self_signed(vec!["updown".to_string()])?;
    let cert_chain = vec![rustls::pki_types::CertificateDer::from(cert.cert)];
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());

    let mut server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key.into())?;
    server_crypto.alpn_protocols = vec![b"updown/1".to_vec()];

    let server_config = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    Ok(server_config)
}

fn make_client_config() -> ClientConfig {
    let mut client_crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerification))
        .with_no_client_auth();
    client_crypto.alpn_protocols = vec![b"updown/1".to_vec()];

    ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto).unwrap(),
    ))
}

/// Skip TLS certificate verification (P2P self-signed context)
#[derive(Debug)]
struct SkipVerification;

impl rustls::client::danger::ServerCertVerifier for SkipVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

/// Control channel server (runs on the receiver side)
pub struct ControlServer {
    endpoint: Endpoint,
}

impl ControlServer {
    pub async fn bind(addr: SocketAddr) -> Result<Self> {
        let server_config = make_server_config()?;
        let endpoint = Endpoint::server(server_config, addr)
            .context("failed to bind QUIC control server")?;
        info!("Control server listening on {}", endpoint.local_addr()?);
        Ok(Self { endpoint })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    /// Accept one incoming connection and exchange control messages
    pub async fn accept(&self) -> Result<ControlConnection> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .context("no incoming connection")?;
        let conn = incoming.await.context("connection failed")?;
        info!("Control connection from {}", conn.remote_address());

        let (send, recv) = conn
            .accept_bi()
            .await
            .context("failed to accept bi stream")?;

        Ok(ControlConnection { send, recv })
    }
}

/// Control channel client (runs on the sender side)
pub struct ControlClient;

impl ControlClient {
    pub async fn connect(server_addr: SocketAddr) -> Result<ControlConnection> {
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(make_client_config());

        let conn = endpoint
            .connect(server_addr, "updown")?
            .await
            .context("QUIC connection failed")?;

        info!("Connected to control server at {}", conn.remote_address());

        let (send, recv) = conn
            .open_bi()
            .await
            .context("failed to open bi stream")?;

        Ok(ControlConnection { send, recv })
    }
}

/// Bidirectional control channel connection
pub struct ControlConnection {
    send: SendStream,
    recv: RecvStream,
}

impl ControlConnection {
    /// Send a control message
    pub async fn send_msg(&mut self, msg: &ControlMessage) -> Result<()> {
        let data = bincode::serialize(msg)?;
        let len = data.len() as u32;
        self.send.write_all(&len.to_le_bytes()).await?;
        self.send.write_all(&data).await?;
        Ok(())
    }

    /// Receive a control message
    pub async fn recv_msg(&mut self) -> Result<ControlMessage> {
        let mut len_buf = [0u8; 4];
        self.recv.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;

        if len > 10 * 1024 * 1024 {
            anyhow::bail!("control message too large: {} bytes", len);
        }

        let mut data = vec![0u8; len];
        self.recv.read_exact(&mut data).await?;
        let msg = bincode::deserialize(&data)?;
        Ok(msg)
    }
}
