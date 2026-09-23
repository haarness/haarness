//! Loopback redirect listener for the PKCE flow (DESIGN.md §5.2, §8.1): a `tokio::net::TcpListener` on
//! `127.0.0.1:<random port>` that parses exactly one raw HTTP/1.1 request line + headers by hand (no `hyper`),
//! answers with a static "You can close this tab" page and hands back `code` + `state`.

use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::{Error, Result};

/// The only path the listener accepts: `redirect_uri = http://127.0.0.1:{port}/callback`.
pub const CALLBACK_PATH: &str = "/callback";
/// Body of the response the browser receives.
pub const CLOSE_TAB_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Haarness Cloud</title></head><body><p>Signed in. You can close this tab.</p></body></html>";
/// Maximum bytes of request head the parser reads before giving up.
pub const MAX_HEAD_BYTES: usize = 16 * 1024;
/// How long [`Loopback::wait_for_callback`] waits for the browser before giving up.
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
/// How long one connection may take to send its request head.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Query parameters of the successful callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallbackParams {
    /// `hcc_…` authorization code.
    pub code: String,
    /// Echoed `state`; the listener already verified it equals the expected one.
    pub state: String,
}

/// The listener and the port to embed in `redirect_uri`.
#[derive(Debug)]
pub struct Loopback {
    pub listener: TcpListener,
    pub port: u16,
}

/// What one request head means to the listener.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Callback {
    /// A valid `/callback?code=…&state=…`.
    Accepted(CallbackParams),
    /// A request the browser sent that is not the callback (`/favicon.ico`, a probe): answer 404, keep waiting.
    Ignored(&'static str),
    /// The callback itself failed (`error=…`, wrong state, no code): answer 400 and give up.
    Rejected(String),
}

impl Loopback {
    /// Binds `127.0.0.1:0` (random port).
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Loopback { listener, port })
    }

    /// `http://127.0.0.1:{port}/callback`.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.port)
    }

    /// Accepts connections until one carries a valid callback (or an `error=` redirect, reported as
    /// [`Error::Other`]); answers every request with [`CLOSE_TAB_HTML`].
    pub async fn wait_for_callback(self, expected_state: &str) -> Result<CallbackParams> {
        tokio::time::timeout(CALLBACK_TIMEOUT, self.accept_loop(expected_state))
            .await
            .map_err(|_| Error::Other("timed out waiting for the browser redirect".to_owned()))?
    }

    async fn accept_loop(self, expected_state: &str) -> Result<CallbackParams> {
        loop {
            let (mut stream, peer) = self.listener.accept().await?;
            if !peer.ip().is_loopback() {
                let _ = stream.shutdown().await;
                continue;
            }
            let Ok(Ok(head)) = tokio::time::timeout(READ_TIMEOUT, read_head(&mut stream)).await
            else {
                let _ = stream.shutdown().await;
                continue;
            };
            match classify(&head, expected_state) {
                Callback::Accepted(params) => {
                    respond(&mut stream, "200 OK", CLOSE_TAB_HTML).await?;
                    return Ok(params);
                }
                Callback::Ignored(_) => {
                    respond(&mut stream, "404 Not Found", "not found").await?;
                }
                Callback::Rejected(reason) => {
                    respond(&mut stream, "400 Bad Request", &error_page(&reason)).await?;
                    return Err(Error::Other(reason));
                }
            }
        }
    }
}

/// Parses one raw HTTP/1.1 request head (`GET /callback?code=…&state=… HTTP/1.1\r\nHost: …\r\n\r\n`).
/// Rejects any method but GET, any path but [`CALLBACK_PATH`], a missing `code`, a `state` that differs from
/// `expected_state`, and turns `error=<code>` into `Err(Error::Other)`.
pub fn parse_callback_request(head: &str, expected_state: &str) -> Result<CallbackParams> {
    match classify(head, expected_state) {
        Callback::Accepted(params) => Ok(params),
        Callback::Ignored(reason) => Err(Error::Other(reason.to_owned())),
        Callback::Rejected(reason) => Err(Error::Other(reason)),
    }
}

fn classify(head: &str, expected_state: &str) -> Callback {
    let Some(request_line) = head.split("\r\n").next().filter(|line| !line.is_empty()) else {
        return Callback::Ignored("empty request");
    };
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Callback::Ignored("malformed request line");
    };
    if method != "GET" {
        return Callback::Ignored("only GET is answered");
    }
    // The request target is origin-form ("/callback?…"); resolve it against a dummy authority to split it.
    let Ok(url) = url::Url::options()
        .base_url(Some(
            &url::Url::parse("http://127.0.0.1/").expect("static base url"),
        ))
        .parse(target)
    else {
        return Callback::Ignored("unparseable request target");
    };
    if url.path() != CALLBACK_PATH {
        return Callback::Ignored("not the callback path");
    }

    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "error_description" => description = Some(value.into_owned()),
            _ => {}
        }
    }

    if let Some(error) = error {
        let detail = description.map_or_else(String::new, |text| format!(": {text}"));
        return Callback::Rejected(format!("authorization failed ({error}){detail}"));
    }
    let Some(state) = state else {
        return Callback::Rejected("the redirect carried no state".to_owned());
    };
    if state != expected_state {
        return Callback::Rejected("the redirect carried a different state".to_owned());
    }
    let Some(code) = code.filter(|code| !code.is_empty()) else {
        return Callback::Rejected("the redirect carried no code".to_owned());
    };
    Callback::Accepted(CallbackParams { code, state })
}

/// Reads up to [`MAX_HEAD_BYTES`] until the end of the request head (`\r\n\r\n`).
async fn read_head(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_head_end(&buffer) {
            buffer.truncate(end);
            break;
        }
        if buffer.len() >= MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head is too large",
            ));
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|start| start + 2)
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

fn error_page(reason: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Haarness Cloud</title></head><body><p>Sign-in failed: {}</p></body></html>",
        reason.replace('&', "&amp;").replace('<', "&lt;")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(target: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nUser-Agent: test\r\n")
    }

    #[test]
    fn accepts_the_callback() {
        let params =
            parse_callback_request(&head("/callback?code=hcc_abc&state=st-1"), "st-1").unwrap();
        assert_eq!(
            params,
            CallbackParams {
                code: "hcc_abc".to_owned(),
                state: "st-1".to_owned()
            }
        );
    }

    #[test]
    fn rejects_wrong_state_missing_code_and_errors() {
        for (target, needle) in [
            ("/callback?code=hcc_abc&state=other", "different state"),
            ("/callback?state=st-1", "no code"),
            ("/callback?code=hcc_abc", "no state"),
            (
                "/callback?error=access_denied&error_description=user%20said%20no&state=st-1",
                "access_denied",
            ),
        ] {
            let error = parse_callback_request(&head(target), "st-1").unwrap_err();
            assert!(error.to_string().contains(needle), "{target}: {error}");
        }
    }

    #[test]
    fn ignores_other_requests() {
        for target in ["/favicon.ico", "/callback/../x", "/"] {
            assert!(parse_callback_request(&head(target), "st-1").is_err());
        }
        let error = parse_callback_request("POST /callback?code=a&state=st-1 HTTP/1.1\r\n", "st-1")
            .unwrap_err();
        assert!(error.to_string().contains("only GET"));
        assert!(parse_callback_request("", "st-1").is_err());
    }

    #[test]
    fn head_end_is_the_blank_line() {
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(16));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n"), None);
    }
}
