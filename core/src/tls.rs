//! TLS 1.3 both ways with self-signed certificates. Handshake signatures are still verified, so a
//! peer must hold its certificate's key; whether to trust that certificate is decided by the
//! session, which compares its fingerprint with the paired one before any data moves.

use crate::identity::{fingerprint, Identity};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::{client, server, TlsAcceptor, TlsConnector};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Marks a connection as the camera stream. The main connection offers no protocol.
const VIDEO_ALPN: &[u8] = b"lis-video/1";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

fn cert_and_key(identity: &Identity) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    (
        vec![CertificateDer::from(identity.cert_der.clone())],
        PrivateKeyDer::from(PrivatePkcs8KeyDer::from(identity.key_der.clone())),
    )
}

fn no_tls12() -> Error {
    Error::General("only TLS 1.3 is used".into())
}

#[derive(Debug)]
struct AnyServerCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(no_tls12())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[derive(Debug)]
struct AnyClientCert(Arc<CryptoProvider>);

impl ClientCertVerifier for AnyClientCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(no_tls12())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

pub fn acceptor(identity: &Identity) -> Result<TlsAcceptor, Error> {
    let provider = provider();
    let (certs, key) = cert_and_key(identity);
    let mut config = ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(AnyClientCert(provider)))
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![VIDEO_ALPN.to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

fn client_config(identity: &Identity) -> Result<ClientConfig, Error> {
    let provider = provider();
    let (certs, key) = cert_and_key(identity);
    ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyServerCert(provider)))
        .with_client_auth_cert(certs, key)
}

pub fn connector(identity: &Identity) -> Result<TlsConnector, Error> {
    Ok(TlsConnector::from(Arc::new(client_config(identity)?)))
}

pub fn video_connector(identity: &Identity) -> Result<TlsConnector, Error> {
    let mut config = client_config(identity)?;
    config.alpn_protocols = vec![VIDEO_ALPN.to_vec()];
    Ok(TlsConnector::from(Arc::new(config)))
}

pub fn server_is_video(stream: &client::TlsStream<TcpStream>) -> bool {
    stream.get_ref().1.alpn_protocol() == Some(VIDEO_ALPN)
}

pub fn client_is_video(stream: &server::TlsStream<TcpStream>) -> bool {
    stream.get_ref().1.alpn_protocol() == Some(VIDEO_ALPN)
}

fn peer_fingerprint(certs: Option<&[CertificateDer<'_>]>) -> io::Result<String> {
    certs
        .and_then(|certs| certs.first())
        .map(|cert| fingerprint(cert.as_ref()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "peer sent no certificate"))
}

/// Returns the stream and the server's fingerprint.
pub async fn connect(
    connector: &TlsConnector,
    address: &str,
    port: u16,
) -> io::Result<(client::TlsStream<TcpStream>, String)> {
    timeout(HANDSHAKE_TIMEOUT, async {
        let tcp = TcpStream::connect((address, port)).await?;
        tcp.set_nodelay(true)?;
        let name = ServerName::try_from("lis-app").map_err(io::Error::other)?;
        let stream = connector.connect(name, tcp).await?;
        let peer = peer_fingerprint(stream.get_ref().1.peer_certificates())?;
        Ok((stream, peer))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connection timed out"))?
}

/// Returns the stream and the client's fingerprint.
pub async fn accept(
    acceptor: &TlsAcceptor,
    tcp: TcpStream,
) -> io::Result<(server::TlsStream<TcpStream>, String)> {
    tcp.set_nodelay(true)?;
    timeout(HANDSHAKE_TIMEOUT, async {
        let stream = acceptor.accept(tcp).await?;
        let peer = peer_fingerprint(stream.get_ref().1.peer_certificates())?;
        Ok((stream, peer))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))?
}
