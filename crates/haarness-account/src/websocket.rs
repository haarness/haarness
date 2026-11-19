//! WebSockets to Haarness Cloud (the remote-control relay, billing and Cloud
//! updates). HTTPS requests go through reqwest, which checks Cloud's
//! certificate with the operating system's verifier; these sockets check it
//! the same way. With tokio-tungstenite's bundled Mozilla roots instead, a
//! network that inspects TLS with a root of its own (a company proxy or
//! security product the system trusts) let every HTTPS request through and
//! refused remote control with "UnknownIssuer".

use std::{
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    time::Instant,
};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream,
    tungstenite::{
        self, client::IntoClientRequest, error::UrlError, handshake::client::Response,
        protocol::WebSocketConfig,
    },
};

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
/// A [`Socket`] that notes when it last heard from the server ([`Heard`]).
pub type HeardSocket = WebSocketStream<MaybeTlsStream<Heard<TcpStream>>>;

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

/// Open a WebSocket as [`connect`] does, on a connection that notes when it
/// last read anything from the server: a socket yields a message only once
/// the whole of it has arrived, so a large one on a slow link looks like a
/// silent server to anyone who only counts messages.
pub async fn connect_heard<R>(
    request: R,
    config: Option<WebSocketConfig>,
) -> Result<(HeardSocket, Response, LastHeard), tungstenite::Error>
where
    R: IntoClientRequest + Unpin,
{
    let request = request.into_client_request()?;
    let uri = request.uri();
    // An IPv6 address comes in brackets, which the connect does not take.
    let host = uri
        .host()
        .ok_or(tungstenite::Error::Url(UrlError::NoHostName))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = uri
        .port_u16()
        .or_else(|| match uri.scheme_str() {
            Some("wss") => Some(443),
            Some("ws") => Some(80),
            _ => None,
        })
        .ok_or(tungstenite::Error::Url(UrlError::UnsupportedUrlScheme))?;
    let tls = platform()?;
    let tcp = TcpStream::connect((host.as_str(), port)).await?;
    let heard = LastHeard::now();
    let stream = Heard {
        inner: tcp,
        heard: heard.clone(),
    };
    let (socket, response) = tokio_tungstenite::client_async_tls_with_config(
        request,
        stream,
        config,
        Some(Connector::Rustls(tls)),
    )
    .await?;
    Ok((socket, response, heard))
}

/// When a [`Heard`] connection last read a byte.
#[derive(Clone, Debug)]
pub struct LastHeard(Arc<Mutex<Instant>>);
impl LastHeard {
    fn now() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }
    /// The last time anything arrived, or when the connection opened.
    #[must_use]
    pub fn at(&self) -> Instant {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn mark(&self) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }
}

/// A connection that notes in [`LastHeard`] each time it reads something.
#[derive(Debug)]
pub struct Heard<S> {
    inner: S,
    heard: LastHeard,
}
impl<S: AsyncRead + Unpin> AsyncRead for Heard<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let read = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(read, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.heard.mark();
        }
        read
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for Heard<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
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

    #[tokio::test]
    async fn a_heard_connection_notes_a_message_still_arriving() {
        use tokio::io::AsyncWriteExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let payload = vec![b'x'; 64 * 1024];
        let sent = payload.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            // One text message, written a piece at a time on a slow link.
            let mut frame = vec![0x81, 127];
            frame.extend((sent.len() as u64).to_be_bytes());
            frame.extend(&sent);
            for piece in frame.chunks(8 * 1024) {
                socket.get_mut().write_all(piece).await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            socket
        });
        let (mut socket, _, heard) = connect_heard(url.as_str(), None).await.unwrap();
        let opened = heard.at();
        // Nothing whole has arrived yet, but the connection has heard it.
        let early =
            tokio::time::timeout(std::time::Duration::from_millis(450), socket.next()).await;
        assert!(early.is_err(), "{early:?}");
        assert!(heard.at() >= opened + std::time::Duration::from_millis(200));
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Text(String::from_utf8(payload).unwrap().into())
        );
        drop(server.await.unwrap());
    }
}
