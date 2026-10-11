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
    if auth.loopback_only
        && (!valid_loopback_host(request.headers()) || !valid_loopback_origin(request.headers()))
    {
        auth.log_rejection(&request, Rejection::Forbidden);
        return (StatusCode::FORBIDDEN, "loopback host/origin required").into_response();
    }
    if !auth.loopback_only && !valid_network_origin(request.headers()) {
        auth.log_rejection(&request, Rejection::Forbidden);
        return (StatusCode::FORBIDDEN, "same-host origin required").into_response();
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
    if auth.anonymous {
        request.extensions_mut().insert(AnonymousAccess);
    }
    next.run(request).await
}

fn request_host(headers: &HeaderMap) -> Option<&str> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())?;
    Some(
        host.rsplit_once(':')
            .map_or(host, |(host, _)| host)
            .trim_matches(['[', ']']),
    )
}

fn valid_loopback_host(headers: &HeaderMap) -> bool {
    request_host(headers).is_some_and(|host| matches!(host, "127.0.0.1" | "::1" | "localhost"))
}

fn valid_loopback_origin(headers: &HeaderMap) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|origin| {
            url::Url::parse(origin)
                .ok()
                .and_then(|url| url.host_str().map(ToOwned::to_owned))
                .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "::1" | "localhost"))
        })
}

/// Terminal clients do not send Origin. If a browser frontend connects to an
/// explicitly network-bound server, require its Origin host to match the HTTP
/// Host header so an unrelated web page cannot drive the authenticated API.
fn valid_network_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return request_host(headers).is_some_and(|host| !host.trim().is_empty());
    };
    let Some(host) = request_host(headers) else {
        return false;
    };
    url::Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
        .is_some_and(|origin| origin.eq_ignore_ascii_case(host))
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

    #[test]
    fn network_origin_must_match_the_requested_host() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "192.0.2.10:4096".parse().unwrap());
        assert!(valid_network_origin(&headers));

        headers.insert(header::ORIGIN, "http://192.0.2.10:3000".parse().unwrap());
        assert!(valid_network_origin(&headers));

        headers.insert(header::ORIGIN, "https://example.test".parse().unwrap());
        assert!(!valid_network_origin(&headers));
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
        let router = Router::new()
            .route("/", get(|| async { "ok" }))
            .route_layer(middleware::from_fn_with_state(
                AuthState::with_access("internal", true, &access),
                require_auth,
            ));
        let mut request = Request::builder()
            .uri("/")
            .header(header::HOST, "127.0.0.1:4096");
        if let Some(value) = authorization {
            request = request.header(header::AUTHORIZATION, value);
        }
        if let Some(value) = origin {
            request = request.header(header::ORIGIN, value);
        }
        router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }
    #[tokio::test]
    async fn open_access_still_rejects_cross_origin_requests() {
        assert_eq!(status(ServerAccess::Open, None, None).await, StatusCode::OK);
        assert_eq!(
            status(ServerAccess::Open, None, Some("https://unrelated.example")).await,
            StatusCode::FORBIDDEN
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
        for (authorization, origin, path, expected) in [
            (None, None, "/v1/threads/a?token=PRIVATE_QUERY", 401),
            (Some("Bearer PRIVATE_STALE"), None, "/v1/threads/b", 401),
            (Some(wrong_basic.as_str()), None, "/v1/sessions/c", 401),
            (Some("PRIVATE_BARE_CREDENTIAL"), None, "/v1/threads/d", 401),
            (
                Some("Bearer PRIVATE_TOKEN"),
                Some("https://PRIVATE-ORIGIN.example"),
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
        ] {
            let mut request = Request::builder()
                .uri(path)
                .header(header::HOST, "127.0.0.1:4096")
                .header(header::USER_AGENT, agent.as_bytes())
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50123))));
            if let Some(value) = authorization {
                request = request.header(header::AUTHORIZATION, value);
            }
            if let Some(value) = origin {
                request = request.header(header::ORIGIN, value);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), expected, "{path}");
        }
        let lines = capture.matching("request rejected");
        assert_eq!(lines.len(), 5, "{:#?}", capture.lines());
        for (line, expected) in lines.iter().zip([
            ["path=/v1/threads/a ", "reason=missing", "scheme=none"],
            [
                "path=/v1/threads/b",
                "reason=bad_credential",
                "scheme=Bearer",
            ],
            [
                "path=/v1/sessions/c",
                "reason=bad_credential",
                "scheme=Basic",
            ],
            [
                "path=/v1/threads/d",
                "reason=wrong_scheme",
                "scheme=unrecognized",
            ],
            ["path=/v1/threads/e", "reason=forbidden", "scheme=Bearer"],
        ]) {
            assert!(line.starts_with("WARN "), "{line}");
            for expected in expected
                .into_iter()
                .chain([" method=GET", " remote=127.0.0.1:50123"])
            {
                assert!(line.contains(expected), "{expected}: {line}");
            }
            assert!(line.contains(" user_agent=fixture-client/1.0 [31m agent agent"));
            assert!(line.contains('…') && !line.contains('\u{9b}'), "{line}");
            assert!(!line.contains("PRIVATE") && !line.contains('?'), "{line}");
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
