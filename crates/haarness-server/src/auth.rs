use std::collections::{HashMap, hash_map::Entry};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{ConnectInfo, OriginalUri, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};

/// Sent by browsers on every request: `none` for the user's own navigation,
/// `same-origin`, `same-site` or `cross-site` for what a page started.
const SEC_FETCH_SITE: &str = "sec-fetch-site";

#[derive(Clone, Default)]
pub enum ServerAccess {
    #[default]
    Private,
    Open,
    Password {
        username: String,
        password: String,
    },
}

impl ServerAccess {
    #[must_use]
    pub fn fingerprint(&self, token: &str) -> String {
        let value = match self {
            Self::Private => serde_json::json!(["private", token]),
            Self::Open => serde_json::json!(["open", token]),
            Self::Password { username, password } => {
                serde_json::json!(["password", token, username, password])
            }
        };
        format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
    }
}

/// Set on requests a server admitted without credentials (Open access), for
/// endpoints that must never be anonymous.
#[derive(Clone, Copy, Debug)]
pub struct AnonymousAccess;

/// Sent by a front ([`crate::front`]) on a request it admitted without
/// credentials: the server behind it, which sees the front's token, treats
/// the request as anonymous all the same. It can only take access away, so
/// any client may send it.
pub const ANONYMOUS_HEADER: &str = "x-haarness-anonymous";

#[derive(Clone)]
pub struct AuthState {
    token_digest: [u8; 32],
    loopback_only: bool,
    anonymous: bool,
    password_digest: Option<[u8; 32]>,
    rejections: Arc<RejectionLog>,
}

impl AuthState {
    #[must_use]
    pub fn new(token: &str, loopback_only: bool) -> Self {
        Self::with_access(token, loopback_only, &ServerAccess::Private)
    }

    #[must_use]
    pub fn with_access(token: &str, loopback_only: bool, access: &ServerAccess) -> Self {
        Self {
            anonymous: matches!(access, ServerAccess::Open),
            password_digest: match access {
                ServerAccess::Password { username, password } => Some(
                    Sha256::digest(STANDARD.encode(format!("{username}:{password}")).as_bytes())
                        .into(),
                ),
                _ => None,
            },
            token_digest: Sha256::digest(token.as_bytes()).into(),
            loopback_only,
            rejections: Arc::default(),
        }
    }

    fn verify(&self, provided: Option<&HeaderValue>) -> Result<(), Rejection> {
        let value = provided
            .ok_or(Rejection::Missing)?
            .to_str()
            .map_err(|_| Rejection::WrongScheme)?;
        if let Some(token) = value.strip_prefix("Bearer ") {
            return constant_time_equal(&Sha256::digest(token.as_bytes()), &self.token_digest)
                .then_some(())
                .ok_or(Rejection::BadCredential);
        }
        match (value.strip_prefix("Basic "), self.password_digest.as_ref()) {
            (Some(value), Some(expected)) => {
                constant_time_equal(&Sha256::digest(value.as_bytes()), expected)
                    .then_some(())
                    .ok_or(Rejection::BadCredential)
            }
            _ => Err(Rejection::WrongScheme),
        }
    }

    /// One WARN per path prefix and reason each minute, so a client polling
    /// with a stale token cannot flood the log. Names the scheme, never a
    /// header value.
    fn log_rejection(&self, request: &Request<Body>, reason: Rejection) {
        let path = request
            .extensions()
            .get::<OriginalUri>()
            .map_or_else(|| request.uri().path(), |uri| uri.path());
        // Share links carry their capability in the path.
        let path = if path.starts_with("/s/") {
            "/s/{token}"
        } else {
            path
        };
        let prefix: String = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .take(2)
            .fold(String::new(), |prefix, segment| prefix + "/" + segment)
            .chars()
            .take(64)
            .collect();
        let Some(suppressed) = self.rejections.admit(prefix, reason, Instant::now()) else {
            return;
        };
        let headers = request.headers();
        // A refused browser request names the site that sent it.
        let forbidden = reason == Rejection::Forbidden;
        let origin = forbidden.then(|| origin_host(headers));
        let site = headers
            .get(SEC_FETCH_SITE)
            .filter(|_| forbidden)
            .and_then(|site| {
                ["none", "same-origin", "same-site", "cross-site"]
                    .into_iter()
                    .find(|known| site.as_bytes() == known.as_bytes())
            });
        let agent = headers
            .get(header::USER_AGENT)
            .map(|agent| crate::hooks::log_text(&String::from_utf8_lossy(agent.as_bytes()), 120));
        tracing::warn!(
            method = %request.method(),
            path = %crate::hooks::log_text(path, 200),
            reason = reason.as_str(),
            scheme = auth_scheme(headers.get(header::AUTHORIZATION)),
            remote = request
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(address)| tracing::field::display(address)),
            user_agent = agent.as_deref(),
            origin = origin.as_deref(),
            fetch_site = site,
            suppressed = (suppressed > 0).then_some(suppressed),
            "request rejected"
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Rejection {
    Missing,
    WrongScheme,
    BadCredential,
    Forbidden,
}

impl Rejection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::WrongScheme => "wrong_scheme",
            Self::BadCredential => "bad_credential",
            Self::Forbidden => "forbidden",
        }
    }
}

/// The Authorization scheme for diagnostics: a known scheme name, or
/// `unrecognized` so a bare credential is never mistaken for one.
fn auth_scheme(value: Option<&HeaderValue>) -> &'static str {
    let Some(value) = value else {
        return "none";
    };
    let scheme = value
        .as_bytes()
        .split(|byte| *byte == b' ')
        .next()
        .unwrap_or_default();
    ["Bearer", "Basic", "Digest", "Token", "Negotiate", "NTLM"]
        .into_iter()
        .find(|known| scheme.eq_ignore_ascii_case(known.as_bytes()) && scheme.len() < value.len())
        .unwrap_or("unrecognized")
}

const REJECTION_WINDOW: Duration = Duration::from_secs(60);
/// Unauthenticated callers choose the path, so the remembered prefixes are capped.
const REJECTION_KEYS: usize = 256;

#[derive(Default)]
struct RejectionLog(parking_lot::Mutex<HashMap<(String, Rejection), (Instant, u64)>>);

impl RejectionLog {
    /// `Some(held back since the last line)` when this rejection may be logged.
    fn admit(&self, prefix: String, reason: Rejection, now: Instant) -> Option<u64> {
        let mut seen = self.0.lock();
        let mut key = (prefix, reason);
        if !seen.contains_key(&key) && seen.len() >= REJECTION_KEYS {
            seen.retain(|_, (last, _)| now.duration_since(*last) < REJECTION_WINDOW);
            if seen.len() >= REJECTION_KEYS {
                key = ("*".into(), reason);
            }
        }
        match seen.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert((now, 0));
                Some(0)
            }
            Entry::Occupied(mut entry) => {
                let (last, held) = entry.get_mut();
                if now.duration_since(*last) < REJECTION_WINDOW {
                    *held += 1;
                    return None;
                }
                *last = now;
                Some(std::mem::take(held))
            }
        }
    }
}

pub async fn require_auth(
    State(auth): State<AuthState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    // Before the credential check: a browser sends a saved password, and an
    // open server has none, along with any request a page makes.
    if let Some(refusal) = refuse_browser_request(request.headers(), auth.loopback_only) {
        auth.log_rejection(&request, Rejection::Forbidden);
        return (StatusCode::FORBIDDEN, refusal).into_response();
    }
    let verdict = if auth.anonymous {
        Ok(())
    } else {
        auth.verify(request.headers().get(header::AUTHORIZATION))
    };
    if let Err(reason) = verdict {
        auth.log_rejection(&request, reason);
        let mut response = (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
        if auth.password_digest.is_some() {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                "Basic realm=\"Haarness\", charset=\"UTF-8\""
                    .parse()
                    .expect("static challenge"),
            );
        }
        return response;
    }
    if auth.anonymous || request.headers().contains_key(ANONYMOUS_HEADER) {
        request.extensions_mut().insert(AnonymousAccess);
    }
    next.run(request).await
}

/// Why a request must not reach the routes, judged from what browsers add.
/// The CLI, TUI, Desktop (whose main process makes every request) and the
/// Cloud relay send neither Origin nor Sec-Fetch-Site, so only Host applies
/// to them.
fn refuse_browser_request(headers: &HeaderMap, loopback_only: bool) -> Option<&'static str> {
    let origin = headers.get(header::ORIGIN);
    let site = headers.get(SEC_FETCH_SITE).map(HeaderValue::as_bytes);
    // An image, link, frame or no-cors fetch from another site carries no
    // Origin; the browser still says where it came from.
    if origin.is_none() && !matches!(site, None | Some(b"none" | b"same-origin")) {
        return Some("cross-site request refused");
    }
    if loopback_only {
        return (!valid_loopback_host(headers) || !valid_loopback_origin(headers))
            .then_some("loopback host/origin required");
    }
    (!valid_network_origin(headers)).then_some("same-host origin required")
}

/// The Host header split into a lowercase name (IPv6 without brackets) and
/// its port, if one is written.
fn request_host(headers: &HeaderMap) -> Option<(String, Option<u16>)> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())?
        .trim();
    let (name, port) = if let Some(rest) = host.strip_prefix('[') {
        let (name, rest) = rest.split_once(']')?;
        match rest {
            "" => (name, None),
            rest => (name, Some(rest.strip_prefix(':')?)),
        }
    } else {
        match host.split_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (host, None),
        }
    };
    let port = port.map(str::parse).transpose().ok()?;
    Some((name.to_ascii_lowercase(), port))
}

fn valid_loopback_host(headers: &HeaderMap) -> bool {
    request_host(headers).is_some_and(|(host, _)| is_loopback_name(&host))
}

fn is_loopback_name(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

/// A browser's Origin on a loopback listener: a loopback name on the
/// server's own port. Another local port is another web site (a dev server, a
/// page some program serves), which must not drive this one.
fn valid_loopback_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(origin) = origin
        .to_str()
        .ok()
        .and_then(|origin| url::Url::parse(origin).ok())
    else {
        return false;
    };
    let port = request_host(headers).and_then(|(_, port)| port);
    origin
        .host_str()
        .is_some_and(|host| is_loopback_name(host.trim_matches(['[', ']'])))
        && same_port(&origin, port)
}

/// Whether an Origin's port is the one the Host header names, a missing one
/// being the Origin scheme's default.
fn same_port(origin: &url::Url, port: Option<u16>) -> bool {
    let default = url::Url::parse(&format!("{}://h", origin.scheme()))
        .ok()
        .and_then(|url| url.port_or_known_default());
    origin.port_or_known_default() == port.or(default)
}

/// Terminal clients send no browser headers, and any Host they name is
/// accepted (a reverse proxy may rename it). A browser request (an Origin, or
/// a Sec-Fetch-Site other than the user's own navigation) must name the server
/// by address, so a web site whose name was re-pointed at this server (DNS
/// rebinding) is refused, and its Origin must match that Host, port included.
fn valid_network_origin(headers: &HeaderMap) -> bool {
    let Some((host, port)) = request_host(headers) else {
        return false;
    };
    if host.is_empty() {
        return false;
    }
    let origin = headers.get(header::ORIGIN);
    let browser = origin.is_some()
        || headers
            .get(SEC_FETCH_SITE)
            .is_some_and(|site| site.as_bytes() != b"none");
    if !browser {
        return true;
    }
    if !is_loopback_name(&host) && host.parse::<std::net::IpAddr>().is_err() {
        return false;
    }
    let Some(origin) = origin else {
        return true;
    };
    let Some(origin) = origin
        .to_str()
        .ok()
        .and_then(|origin| url::Url::parse(origin).ok())
    else {
        return false;
    };
    let named = origin
        .host_str()
        .is_some_and(|name| name.trim_matches(['[', ']']).eq_ignore_ascii_case(&host));
    named && same_port(&origin, port)
}

/// What a refused browser request names as its site, for the log: the Origin's
/// host, `opaque` for `null` or an unreadable one, or `none`.
fn origin_host(headers: &HeaderMap) -> String {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return "none".into();
    };
    origin
        .to_str()
        .ok()
        .and_then(|origin| url::Url::parse(origin).ok())
        .and_then(|url| {
            let host = url.host_str()?;
            Some(match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            })
        })
        .map_or_else(|| "opaque".into(), |host| crate::hooks::log_text(&host, 80))
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comparison_does_not_accept_prefixes() {
        assert!(constant_time_equal(b"same", b"same"));
        assert!(!constant_time_equal(b"same", b"sam"));
        assert!(!constant_time_equal(b"same", b"sand"));
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    #[test]
    fn network_origin_must_match_the_requested_host_and_port() {
        let valid = |pairs: &[(&str, &str)]| valid_network_origin(&headers(pairs));
        assert!(valid(&[("host", "192.0.2.10:4096")]));
        assert!(valid(&[
            ("host", "192.0.2.10:4096"),
            ("origin", "http://192.0.2.10:4096")
        ]));
        assert!(valid(&[
            ("host", "[2001:db8::1]:4096"),
            ("origin", "http://[2001:DB8::1]:4096")
        ]));
        assert!(valid(&[
            ("host", "192.0.2.10"),
            ("origin", "http://192.0.2.10")
        ]));
        assert!(valid(&[
            ("host", "192.0.2.10:443"),
            ("origin", "https://192.0.2.10")
        ]));
        assert!(valid(&[
            ("host", "localhost:4096"),
            ("origin", "http://localhost:4096")
        ]));
        assert!(valid(&[
            ("host", "[::1]"),
            ("sec-fetch-site", "same-origin")
        ]));
        // Another port is another web site.
        assert!(!valid(&[
            ("host", "192.0.2.10:4096"),
            ("origin", "http://192.0.2.10:3000")
        ]));
        assert!(!valid(&[
            ("host", "192.0.2.10"),
            ("origin", "http://192.0.2.10:8080")
        ]));
        assert!(!valid(&[
            ("host", "192.0.2.10:4096"),
            ("origin", "https://example.test")
        ]));
        assert!(!valid(&[("host", "192.0.2.10:4096"), ("origin", "null")]));
        assert!(!valid(&[("origin", "http://192.0.2.10:4096")]));
        // DNS rebinding: the attacker's name now points at this server, so
        // Host and Origin agree; only a name that is an address is trusted.
        assert!(!valid(&[
            ("host", "attacker.example:4096"),
            ("origin", "http://attacker.example:4096")
        ]));
        assert!(!valid(&[
            ("host", "attacker.example:4096"),
            ("sec-fetch-site", "same-origin")
        ]));
        // Terminal clients behind a proxy, and the user's own navigation.
        assert!(valid(&[("host", "haarness.internal:4096")]));
        assert!(valid(&[
            ("host", "haarness.internal:4096"),
            ("sec-fetch-site", "none")
        ]));
        assert!(!valid(&[("host", "192.0.2.10:port")]));
        assert!(!valid(&[("host", "[2001:db8::1")]));
    }

    #[test]
    fn host_header_keeps_ipv6_addresses_whole() {
        let host = |value: &str| request_host(&headers(&[("host", value)]));
        assert_eq!(host("[::1]:4096"), Some(("::1".into(), Some(4096))));
        assert_eq!(host("[fe80::1]"), Some(("fe80::1".into(), None)));
        assert_eq!(host("LocalHost:80"), Some(("localhost".into(), Some(80))));
        assert_eq!(host("[::1]4096"), None);
    }
}

#[cfg(test)]
mod access_tests {
    use super::*;
    use axum::{Router, middleware, routing::get};
    use tower::ServiceExt;

    async fn status(
        access: ServerAccess,
        authorization: Option<&str>,
        origin: Option<&str>,
    ) -> StatusCode {
        let mut headers = vec![("host", "127.0.0.1:4096")];
        headers.extend(authorization.map(|value| ("authorization", value)));
        headers.extend(origin.map(|value| ("origin", value)));
        status_on(access, true, &headers).await
    }

    async fn status_on(
        access: ServerAccess,
        loopback_only: bool,
        headers: &[(&str, &str)],
    ) -> StatusCode {
        let router = Router::new()
            .route("/", get(|| async { "ok" }))
            .route_layer(middleware::from_fn_with_state(
                AuthState::with_access("internal", loopback_only, &access),
                require_auth,
            ));
        let mut request = Request::builder().uri("/");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }
    /// A front admits an anonymous caller and comes back with this server's
    /// token: what must never be anonymous stays closed to that caller.
    #[tokio::test]
    async fn a_front_marked_request_counts_as_anonymous_despite_its_token() {
        let router = Router::new()
            .route(
                "/",
                get(
                    |anonymous: Option<axum::Extension<AnonymousAccess>>| async move {
                        if anonymous.is_some() {
                            "anonymous"
                        } else {
                            "named"
                        }
                    },
                ),
            )
            .route_layer(middleware::from_fn_with_state(
                AuthState::with_access("internal", true, &ServerAccess::Private),
                require_auth,
            ));
        let call = |marked: bool| {
            let mut request = Request::builder()
                .uri("/")
                .header("host", "127.0.0.1:4096")
                .header("authorization", "Bearer internal");
            if marked {
                request = request.header(ANONYMOUS_HEADER, "1");
            }
            router.clone().oneshot(request.body(Body::empty()).unwrap())
        };
        for (marked, expected) in [(false, "named"), (true, "anonymous")] {
            let body = axum::body::to_bytes(call(marked).await.unwrap().into_body(), 64)
                .await
                .unwrap();
            assert_eq!(body, expected);
        }
    }
    #[tokio::test]
    async fn open_access_still_rejects_cross_origin_requests() {
        assert_eq!(status(ServerAccess::Open, None, None).await, StatusCode::OK);
        assert_eq!(
            status(ServerAccess::Open, None, Some("https://unrelated.example")).await,
            StatusCode::FORBIDDEN
        );
    }
    /// A page on another local port (`http://localhost:3000`) is another web
    /// site: on a plain open `haarness serve` it could otherwise POST
    /// `/v1/shutdown` with `mode: "no-cors"`.
    #[tokio::test]
    async fn loopback_server_refuses_an_origin_on_another_local_port() {
        let host = ("host", "127.0.0.1:4096");
        for (origin, expected) in [
            ("http://localhost:3000", StatusCode::FORBIDDEN),
            ("http://127.0.0.1:3000", StatusCode::FORBIDDEN),
            ("http://[::1]:4097", StatusCode::FORBIDDEN),
            ("http://127.0.0.1", StatusCode::FORBIDDEN),
            ("null", StatusCode::FORBIDDEN),
            ("http://127.0.0.1:4096", StatusCode::OK),
            ("http://localhost:4096", StatusCode::OK),
        ] {
            let status = status_on(ServerAccess::Open, true, &[host, ("origin", origin)]).await;
            assert_eq!(status, expected, "{origin}");
        }
        // Terminal clients, Desktop's main process and the SDKs send no Origin.
        for host in ["127.0.0.1:4096", "localhost:4096"] {
            let status = status_on(ServerAccess::Open, true, &[("host", host)]).await;
            assert_eq!(status, StatusCode::OK, "{host}");
        }
    }

    /// An image, link or no-cors fetch from a web page carries no Origin, and
    /// on an open server, or with a saved password, it would be admitted.
    #[tokio::test]
    async fn requests_a_page_started_without_origin_are_refused_before_credentials() {
        let basic = format!("Basic {}", STANDARD.encode("alex:secret"));
        let password = ServerAccess::Password {
            username: "alex".into(),
            password: "secret".into(),
        };
        for (access, authorization) in
            [(ServerAccess::Open, None), (password, Some(basic.as_str()))]
        {
            for (loopback_only, host) in [(true, "127.0.0.1:4096"), (false, "192.0.2.10:4096")] {
                let mut base = vec![("host", host)];
                base.extend(authorization.map(|value| ("authorization", value)));
                for (site, expected) in [
                    (None, StatusCode::OK),
                    (Some("none"), StatusCode::OK),
                    (Some("same-origin"), StatusCode::OK),
                    (Some("same-site"), StatusCode::FORBIDDEN),
                    (Some("cross-site"), StatusCode::FORBIDDEN),
                    (Some("Cross-Site"), StatusCode::FORBIDDEN),
                ] {
                    let mut headers = base.clone();
                    headers.extend(site.map(|site| ("sec-fetch-site", site)));
                    assert_eq!(
                        status_on(access.clone(), loopback_only, &headers).await,
                        expected,
                        "{host} {site:?}"
                    );
                }
            }
        }
    }
    /// `--anonymous --host 0.0.0.0`: after DNS rebinding a page's Host and
    /// Origin both name the attacker's site, so their agreeing is not enough.
    #[tokio::test]
    async fn anonymous_network_server_refuses_a_rebound_web_site() {
        let open = |headers: &'static [(&'static str, &'static str)]| {
            status_on(ServerAccess::Open, false, headers)
        };
        assert_eq!(
            open(&[
                ("host", "attacker.example:4096"),
                ("origin", "http://attacker.example:4096"),
            ])
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            open(&[
                ("host", "attacker.example:4096"),
                ("sec-fetch-site", "same-origin"),
            ])
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            open(&[
                ("host", "192.0.2.10:4096"),
                ("origin", "http://192.0.2.10:8080"),
            ])
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            open(&[
                ("host", "192.0.2.10:4096"),
                ("origin", "http://192.0.2.10:4096"),
            ])
            .await,
            StatusCode::OK
        );
        assert_eq!(
            open(&[("host", "attacker.example:4096")]).await,
            StatusCode::OK
        );
    }
    #[tokio::test]
    async fn password_access_checks_both_fields_and_preserves_internal_bearer() {
        let access = ServerAccess::Password {
            username: "alex".into(),
            password: "日本:secret".into(),
        };
        let correct = format!("Basic {}", STANDARD.encode("alex:日本:secret"));
        let wrong_user = format!("Basic {}", STANDARD.encode("other:日本:secret"));
        let wrong_password = format!("Basic {}", STANDARD.encode("alex:wrong"));
        for invalid in [
            None,
            Some("Basic %%%"),
            Some(wrong_user.as_str()),
            Some(wrong_password.as_str()),
            Some("Bearer wrong"),
        ] {
            assert_eq!(
                status(access.clone(), invalid, None).await,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            status(access.clone(), Some(&correct), None).await,
            StatusCode::OK
        );
        assert_eq!(
            status(access, Some("Bearer internal"), None).await,
            StatusCode::OK
        );
        assert_eq!(
            status(ServerAccess::Private, None, None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(ServerAccess::Private, Some(&correct), None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn rejected_requests_log_reason_and_scheme_but_never_credentials() {
        let (capture, _guard) = crate::hooks::log_capture::Capture::install();
        let access = ServerAccess::Password {
            username: "alex".into(),
            password: "PRIVATE_PASSWORD".into(),
        };
        let router = Router::new()
            .route("/v1/threads/{id}", get(|| async { "ok" }))
            .route("/v1/sessions/{id}", get(|| async { "ok" }))
            .route_layer(middleware::from_fn_with_state(
                AuthState::with_access("PRIVATE_TOKEN", true, &access),
                require_auth,
            ));
        // A C1 control is valid header obs-text once UTF-8 encoded.
        let agent = format!("fixture-client/1.0 \u{9b}[31m {}", "agent ".repeat(60));
        let wrong_basic = format!("Basic {}", STANDARD.encode("alex:PRIVATE_WRONG"));
        // The extra header is what a browser adds.
        for (authorization, extra, path, expected) in [
            (None, None, "/v1/threads/a?token=PRIVATE_QUERY", 401),
            (Some("Bearer PRIVATE_STALE"), None, "/v1/threads/b", 401),
            (Some(wrong_basic.as_str()), None, "/v1/sessions/c", 401),
            (Some("PRIVATE_BARE_CREDENTIAL"), None, "/v1/threads/d", 401),
            (
                Some("Bearer PRIVATE_TOKEN"),
                Some(("origin", "https://unrelated.example:8443")),
                "/v1/threads/e",
                403,
            ),
            // The same prefix and reason within a minute is held back.
            (
                Some("Bearer PRIVATE_STALE_AGAIN"),
                None,
                "/v1/threads/f",
                401,
            ),
            (Some("Bearer PRIVATE_TOKEN"), None, "/v1/threads/g", 200),
            (
                Some("Bearer PRIVATE_TOKEN"),
                Some(("sec-fetch-site", "cross-site")),
                "/v1/sessions/h",
                403,
            ),
        ] {
            let mut request = Request::builder()
                .uri(path)
                .header(header::HOST, "127.0.0.1:4096")
                .header(header::USER_AGENT, agent.as_bytes())
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50123))));
            if let Some(value) = authorization {
                request = request.header(header::AUTHORIZATION, value);
            }
            if let Some((name, value)) = extra {
                request = request.header(name, value);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), expected, "{path}");
        }
        let lines = capture.matching("request rejected");
        assert_eq!(lines.len(), 6, "{:#?}", capture.lines());
        for (line, expected) in lines.iter().zip([
            &["path=/v1/threads/a ", "reason=missing", "scheme=none"][..],
            &[
                "path=/v1/threads/b",
                "reason=bad_credential",
                "scheme=Bearer",
            ],
            &[
                "path=/v1/sessions/c",
                "reason=bad_credential",
                "scheme=Basic",
            ],
            &[
                "path=/v1/threads/d",
                "reason=wrong_scheme",
                "scheme=unrecognized",
            ],
            // A refused browser request names the site that sent it.
            &[
                "path=/v1/threads/e",
                "reason=forbidden",
                "scheme=Bearer",
                " origin=unrelated.example:8443",
            ],
            &[
                "path=/v1/sessions/h",
                "reason=forbidden",
                " origin=none",
                " fetch_site=cross-site",
            ],
        ]) {
            assert!(line.starts_with("WARN "), "{line}");
            for expected in expected
                .iter()
                .copied()
                .chain([" method=GET", " remote=127.0.0.1:50123"])
            {
                assert!(line.contains(expected), "{expected}: {line}");
            }
            assert!(line.contains(" user_agent=fixture-client/1.0 [31m agent agent"));
            assert!(line.contains('…') && !line.contains('\u{9b}'), "{line}");
            assert!(!line.contains("PRIVATE") && !line.contains('?'), "{line}");
            assert!(
                line.contains("reason=forbidden") || !line.contains(" origin="),
                "{line}"
            );
        }
    }

    #[test]
    fn rejection_log_admits_one_line_per_prefix_and_reason_each_minute() {
        let log = RejectionLog::default();
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let threads = || "/v1/threads".to_owned();
        assert_eq!(
            log.admit(threads(), Rejection::BadCredential, at(0)),
            Some(0)
        );
        assert_eq!(log.admit(threads(), Rejection::BadCredential, at(1)), None);
        assert_eq!(log.admit(threads(), Rejection::BadCredential, at(59)), None);
        assert_eq!(log.admit(threads(), Rejection::Missing, at(2)), Some(0));
        assert_eq!(
            log.admit("/v1/events".into(), Rejection::BadCredential, at(2)),
            Some(0)
        );
        assert_eq!(
            log.admit(threads(), Rejection::BadCredential, at(61)),
            Some(2)
        );
        assert_eq!(log.admit(threads(), Rejection::BadCredential, at(62)), None);
        // Caller-chosen paths cannot grow the table without bound.
        for index in 0..1_000 {
            log.admit(format!("/probe{index}"), Rejection::Missing, at(62));
        }
        assert!(log.0.lock().len() <= REJECTION_KEYS + 1);
        assert_eq!(
            log.admit("/probe-late".into(), Rejection::Missing, at(63)),
            None
        );
    }

    #[test]
    fn auth_scheme_names_only_known_schemes() {
        let scheme = |value: &str| auth_scheme(Some(&HeaderValue::from_str(value).unwrap()));
        assert_eq!(auth_scheme(None), "none");
        assert_eq!(scheme("Bearer abc"), "Bearer");
        assert_eq!(scheme("bearer abc"), "Bearer");
        assert_eq!(scheme("Basic abc"), "Basic");
        assert_eq!(scheme("Token abc"), "Token");
        assert_eq!(scheme("PRIVATE_BARE abc"), "unrecognized");
        assert_eq!(scheme("PRIVATE_BARE"), "unrecognized");
        assert_eq!(scheme("Bearer"), "unrecognized");
    }
}
