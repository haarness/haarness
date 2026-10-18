//! Loopback redirect listener for the PKCE flow (DESIGN.md §5.2, §8.1): a `tokio::net::TcpListener` on
//! `127.0.0.1:<random port>` that parses raw HTTP/1.1 request lines + headers by hand (no `hyper`),
//! answers with a static "You can close this tab" page and hands back `code` + `state`.

use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::{Error, Result};

/// The only path the listener accepts: `redirect_uri = http://127.0.0.1:{port}/callback`.
pub const CALLBACK_PATH: &str = "/callback";
/// The page's look: Haarness's palette in dark and light, with no external requests.
macro_rules! page_css {
    () => {
        ":root{color-scheme:dark;--page:#151413;--surface:#232120;--raised:#2c2a27;--text:#eae5dd;--muted:#a9a298;--line:#ffffff16;--accent:#dba273;--danger:#ef8078}\
@media (prefers-color-scheme:light){:root{color-scheme:light;--page:#fdfcfa;--surface:#f6f3ee;--raised:#ebe7df;--text:#232120;--muted:#635c53;--line:#00000016;--accent:#a8622c;--danger:#b3382f}}\
*{box-sizing:border-box}html,body{margin:0}\
body{min-height:100vh;display:grid;place-items:center;padding:24px 16px;background:var(--page);color:var(--text);font:14px/1.5 -apple-system,BlinkMacSystemFont,\"Segoe UI\",Inter,Roboto,\"Helvetica Neue\",Arial,sans-serif;-webkit-font-smoothing:antialiased}\
main{width:min(380px,100%);padding:28px;border:1px solid var(--line);border-radius:20px;background:var(--surface);text-align:center}\
.mark{display:grid;place-items:center;width:44px;height:44px;margin:0 auto 18px;border-radius:12px;background:var(--raised);border:1px solid var(--line)}\
.mark svg{width:22px;height:22px}\
h1{margin:0 0 6px;font-size:19px;font-weight:600;letter-spacing:-0.012em}\
p{margin:0;color:var(--muted)}\
.reason{margin-top:16px;padding:10px 12px;border-radius:12px;border:1px solid color-mix(in srgb,var(--danger) 40%,transparent);background:color-mix(in srgb,var(--danger) 10%,transparent);color:var(--text);font-size:13px;text-align:start;overflow-wrap:anywhere}"
    };
}
/// The Haarness mark, drawn inline.
macro_rules! mark {
    () => {
        "<div class=\"mark\"><svg viewBox=\"0 0 64 64\" fill=\"currentColor\" aria-hidden=\"true\"><rect x=\"17\" y=\"14\" width=\"8\" height=\"36\" rx=\"2\"/><rect x=\"39\" y=\"14\" width=\"8\" height=\"36\" rx=\"2\"/><rect x=\"13.5\" y=\"27\" width=\"37\" height=\"8\" rx=\"4\"/></svg></div>"
    };
}
/// Body of the response the browser receives after a successful sign-in.
pub const CLOSE_TAB_HTML: &str = concat!(
    "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">",
    "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
    "<meta name=\"color-scheme\" content=\"dark light\"><title>Signed in — Haarness</title>",
    "<style>",
    page_css!(),
    "</style></head><body><main>",
    mark!(),
    "<h1>You’re signed in</h1>",
    "<p>Return to Haarness to continue. You can close this tab.</p>",
    "</main></body></html>"
);

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
    /// The callback path without this sign-in's `state`, which a web page that found the port can send:
    /// answer 400 with a fixed page and keep waiting, so it can neither end the sign-in nor word its error.
    Foreign,
    /// This sign-in's callback failed (`error=…`, no code): answer 400 and give up.
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

    /// Accepts connections until one carries this sign-in's `state` with a code (or with an `error=`,
    /// reported as [`Error::Other`]); every other request is answered and the wait goes on.
    pub async fn wait_for_callback(self, expected_state: &str) -> Result<CallbackParams> {
        tokio::time::timeout(CALLBACK_TIMEOUT, self.accept_loop(expected_state))
            .await
            .map_err(|_| Error::Other("timed out waiting for the browser redirect".to_owned()))?
    }

    async fn accept_loop(self, expected_state: &str) -> Result<CallbackParams> {
        // Each connection is read in a task of its own while the listener keeps accepting, so idle
        // ones (a browser's spare socket, a page's preconnect) never hold up the real return.
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    let (mut stream, peer) = accepted?;
                    if !peer.ip().is_loopback() {
                        let _ = stream.shutdown().await;
                        continue;
                    }
                    connections.spawn(answer(stream, expected_state.to_owned()));
                }
                Some(Ok(Some(outcome))) = connections.join_next() => return outcome,
            }
        }
    }
}

/// Reads one connection and answers it; `Some` when it ends the sign-in. A browser that closes the
/// tab early loses only its page.
async fn answer(mut stream: TcpStream, expected_state: String) -> Option<Result<CallbackParams>> {
    let Ok(Ok(head)) = tokio::time::timeout(READ_TIMEOUT, read_head(&mut stream)).await else {
        let _ = stream.shutdown().await;
        return None;
    };
    match classify(&head, &expected_state) {
        Callback::Accepted(params) => {
            let _ = respond(&mut stream, "200 OK", CLOSE_TAB_HTML).await;
            Some(Ok(params))
        }
        Callback::Ignored(_) => {
            let _ = respond(&mut stream, "404 Not Found", "not found").await;
            None
        }
        Callback::Foreign => {
            let page = error_page("This is not the sign-in Haarness started.");
            let _ = respond(&mut stream, "400 Bad Request", &page).await;
            None
        }
        Callback::Rejected(reason) => {
            let _ = respond(&mut stream, "400 Bad Request", &error_page(&reason)).await;
            Some(Err(Error::Other(reason)))
        }
    }
}

/// Sorts one raw HTTP/1.1 request head (`GET /callback?code=…&state=… HTTP/1.1\r\nHost: …\r\n\r\n`).
/// Any method but GET or any path but [`CALLBACK_PATH`] is ignored, and a `state` that is missing or
/// differs from `expected_state` is foreign; with this sign-in's state, `error=<code>` or a missing
/// `code` rejects the sign-in.
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

    let Some(state) = state.filter(|state| state == expected_state) else {
        return Callback::Foreign;
    };
    if let Some(error) = error {
        let detail = description.map_or_else(String::new, |text| format!(": {text}"));
        return Callback::Rejected(format!("authorization failed ({error}){detail}"));
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
    // The stylesheet has braces, so this is assembled rather than `format!`ted.
    let mut page = String::from(concat!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">",
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
        "<meta name=\"color-scheme\" content=\"dark light\"><title>Sign-in did not finish — Haarness</title>",
        "<style>",
        page_css!(),
        "</style></head><body><main>",
        mark!(),
        "<h1>Sign-in did not finish</h1>",
        "<p>Close this tab and try again from Haarness.</p>",
        "<p class=\"reason\">"
    ));
    page.push_str(&reason.replace('&', "&amp;").replace('<', "&lt;"));
    page.push_str("</p></main></body></html>");
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(target: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nUser-Agent: test\r\n")
    }

    #[test]
    fn result_pages_are_branded_and_escape_the_reason() {
        assert!(CLOSE_TAB_HTML.contains("<h1>You’re signed in</h1>"));
        assert!(CLOSE_TAB_HTML.contains("prefers-color-scheme:light"));
        let page = error_page("denied <script>&");
        assert!(page.contains("<h1>Sign-in did not finish</h1>"));
        assert!(page.contains("denied &lt;script>&amp;"));
        assert!(!page.contains("<script>"));
    }

    #[test]
    fn accepts_the_callback() {
        assert_eq!(
            classify(&head("/callback?code=hcc_abc&state=st-1"), "st-1"),
            Callback::Accepted(CallbackParams {
                code: "hcc_abc".to_owned(),
                state: "st-1".to_owned()
            })
        );
    }

    #[test]
    fn rejects_this_sign_ins_missing_code_and_errors() {
        for (target, needle) in [
            ("/callback?state=st-1", "no code"),
            (
                "/callback?error=access_denied&error_description=user%20said%20no&state=st-1",
                "access_denied",
            ),
        ] {
            let Callback::Rejected(error) = classify(&head(target), "st-1") else {
                panic!("{target} was not rejected");
            };
            assert!(error.contains(needle), "{target}: {error}");
        }
    }

    /// A web page that finds the port can send the callback path, but not this sign-in's state.
    #[test]
    fn a_callback_without_this_sign_ins_state_is_foreign() {
        for target in [
            "/callback?code=hcc_abc&state=other",
            "/callback?code=hcc_abc",
            "/callback?error=access_denied&error_description=PAGE_TEXT",
            "/callback?error=access_denied&state=other",
        ] {
            assert_eq!(
                classify(&head(target), "st-1"),
                Callback::Foreign,
                "{target}"
            );
        }
    }

    /// Foreign requests and probes are answered while the listener keeps waiting for the real return.
    #[tokio::test]
    async fn the_listener_waits_past_foreign_requests() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let loopback = Loopback::bind().await.unwrap();
        let port = loopback.port;
        let waiting = tokio::spawn(loopback.wait_for_callback("st-1"));
        let send = |target: &'static str| async move {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            stream
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        };
        let response = send("/callback?error=access_denied&error_description=PAGE_TEXT").await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(!response.contains("PAGE_TEXT"), "{response}");
        assert!(send("/favicon.ico").await.starts_with("HTTP/1.1 404"));
        assert!(!waiting.is_finished());
        assert!(
            send("/callback?code=hcc_abc&state=st-1")
                .await
                .starts_with("HTTP/1.1 200")
        );
        assert_eq!(waiting.await.unwrap().unwrap().code, "hcc_abc");
    }

    #[test]
    fn ignores_other_requests() {
        for target in ["/favicon.ico", "/callback/../x", "/"] {
            assert!(
                matches!(classify(&head(target), "st-1"), Callback::Ignored(_)),
                "{target}"
            );
        }
        assert!(matches!(
            classify("POST /callback?code=a&state=st-1 HTTP/1.1\r\n", "st-1"),
            Callback::Ignored(reason) if reason.contains("only GET")
        ));
        assert!(matches!(classify("", "st-1"), Callback::Ignored(_)));
    }

    /// An idle connection (a browser's spare socket, a page's preconnect) does not hold up the
    /// real return for its read timeout.
    #[tokio::test]
    async fn idle_connections_do_not_delay_the_callback() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let loopback = Loopback::bind().await.unwrap();
        let port = loopback.port;
        let waiting = tokio::spawn(loopback.wait_for_callback("st-1"));
        let _idle = [
            TcpStream::connect(("127.0.0.1", port)).await.unwrap(),
            TcpStream::connect(("127.0.0.1", port)).await.unwrap(),
        ];
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(b"GET /callback?code=hcc_abc&state=st-1 HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(3), stream.read_to_string(&mut response))
            .await
            .expect("answered while idle connections wait")
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let params = tokio::time::timeout(Duration::from_secs(3), waiting)
            .await
            .expect("the sign-in ends while idle connections wait")
            .unwrap()
            .unwrap();
        assert_eq!(params.code, "hcc_abc");
    }

    #[test]
    fn head_end_is_the_blank_line() {
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(16));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n"), None);
    }
}
