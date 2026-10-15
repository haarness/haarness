//! WebSockets to Haarness Cloud (the remote-control relay, billing and Cloud
//! updates). HTTPS requests go through reqwest, which checks Cloud's
//! certificate with the operating system's verifier; these sockets check it
//! the same way. With tokio-tungstenite's bundled Mozilla roots instead, a
//! network that inspects TLS with a root of its own (a company proxy or
//! security product the system trusts) let every HTTPS request through and
//! refused remote control with "UnknownIssuer".

use std::sync::{Arc, OnceLock};

use tokio::net::TcpStream;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream,
    tungstenite::{
        self, client::IntoClientRequest, handshake::client::Response, protocol::WebSocketConfig,
    },
};

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Open a WebSocket. A `wss` server's certificate is checked by the
/// operating system's verifier, as reqwest checks HTTPS; `ws` stays plain.
pub async fn connect<R>(
    request: R,
    config: Option<WebSocketConfig>,
) -> Result<(Socket, Response), tungstenite::Error>
where
    R: IntoClientRequest + Unpin,
{
    connect_with(request, config, platform()?).await
}

async fn connect_with<R>(
    request: R,
    config: Option<WebSocketConfig>,
    tls: Arc<rustls::ClientConfig>,
) -> Result<(Socket, Response), tungstenite::Error>
where
    R: IntoClientRequest + Unpin,
{
    let connector = Connector::Rustls(tls);
    tokio_tungstenite::connect_async_tls_with_config(request, config, false, Some(connector)).await
}

/// The TLS settings every Cloud WebSocket shares, built on first use.
fn platform() -> Result<Arc<rustls::ClientConfig>, tungstenite::Error> {
    use rustls_platform_verifier::ConfigVerifierExt as _;
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let config = rustls::ClientConfig::with_platform_verifier()
        .map_err(|error| tungstenite::Error::Tls(error.into()))?;
    Ok(CONFIG.get_or_init(|| Arc::new(config)).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt as _, StreamExt as _};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::{path::Path, process::Command};
    use tokio_tungstenite::tungstenite::Message;

    /// A test root and a `localhost` certificate it signed, made by the
    /// `openssl` command; `None` where it is not installed.
    fn certificates(dir: &Path) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let run = |args: &[&str]| {
            Command::new("openssl")
                .args(args)
                .current_dir(dir)
                .output()
                .is_ok_and(|output| output.status.success())
        };
        std::fs::write(
            dir.join("ca.cnf"),
            "[req]\ndistinguished_name = dn\n[dn]\n[root]\nbasicConstraints = critical,CA:TRUE\nkeyUsage = critical,keyCertSign,cRLSign\nsubjectKeyIdentifier = hash\n",
        )
        .ok()?;
        std::fs::write(
            dir.join("leaf.cnf"),
            "basicConstraints = critical,CA:FALSE\nkeyUsage = critical,digitalSignature\nextendedKeyUsage = serverAuth\nsubjectAltName = DNS:localhost\n",
        )
        .ok()?;
        let made = [
            &[
                "ecparam",
                "-name",
                "prime256v1",
                "-genkey",
                "-noout",
                "-out",
                "ca.key",
            ][..],
            &[
                "req",
                "-x509",
                "-new",
                "-key",
                "ca.key",
                "-sha256",
                "-days",
                "30",
                "-subj",
                "/CN=Haarness test root",
                "-config",
                "ca.cnf",
                "-extensions",
                "root",
                "-out",
                "ca.pem",
            ],
            &[
                "ecparam",
                "-name",
                "prime256v1",
                "-genkey",
                "-noout",
                "-out",
                "leaf.key",
            ],
            &[
                "req",
                "-new",
                "-key",
                "leaf.key",
                "-subj",
                "/CN=localhost",
                "-config",
                "ca.cnf",
                "-out",
                "leaf.csr",
            ],
            &[
                "x509",
                "-req",
                "-in",
                "leaf.csr",
                "-CA",
                "ca.pem",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-days",
                "30",
                "-sha256",
                "-extfile",
                "leaf.cnf",
                "-out",
                "leaf.pem",
            ],
            &["x509", "-in", "ca.pem", "-outform", "DER", "-out", "ca.der"],
            &[
                "x509", "-in", "leaf.pem", "-outform", "DER", "-out", "leaf.der",
            ],
            &[
                "pkcs8",
                "-topk8",
                "-nocrypt",
                "-in",
                "leaf.key",
                "-outform",
                "DER",
                "-out",
                "leaf.key.der",
            ],
        ]
        .iter()
        .all(|args| run(args));
        if !made {
            return None;
        }
        Some((
            std::fs::read(dir.join("ca.der")).ok()?,
            std::fs::read(dir.join("leaf.der")).ok()?,
            std::fs::read(dir.join("leaf.key.der")).ok()?,
        ))
    }

    #[tokio::test]
    async fn cloud_websockets_trust_what_the_platform_verifier_trusts() {
        let dir = std::env::temp_dir().join(format!("haarness-wss-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some((root, leaf, key)) = certificates(&dir) else {
            eprintln!("skipped: the openssl command is not available to make test certificates");
            return;
        };
        let server = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(leaf)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("wss://localhost:{}/", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    // A client that refuses the certificate ends the handshake here.
                    let Ok(stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    if let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await {
                        let _ = socket.send(Message::Text("hello".into())).await;
                        let _ = socket.close(None).await;
                    }
                });
            }
        });

        // Bundled roots know nothing of a root the system was given.
        let bundled = tokio_tungstenite::connect_async(url.as_str())
            .await
            .unwrap_err();
        assert!(bundled.to_string().contains("UnknownIssuer"), "{bundled}");
        // The platform verifier checks the certificate: this root is not
        // trusted on this computer.
        assert!(connect(url.as_str(), None).await.is_err());
        // Trusted the way a company root is (here an extra root, without a
        // keychain), the same verifier lets the socket open.
        let provider = rustls::ClientConfig::builder().crypto_provider().clone();
        let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(
            [CertificateDer::from(root)],
            provider,
        )
        .unwrap();
        let trusting = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        let (mut socket, _) = connect_with(url.as_str(), None, Arc::new(trusting))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Text("hello".into())
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
