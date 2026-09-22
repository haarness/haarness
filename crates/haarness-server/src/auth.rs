use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
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

#[derive(Clone)]
pub struct AuthState {
    token_digest: [u8; 32],
    loopback_only: bool,
    anonymous: bool,
    password_digest: Option<[u8; 32]>,
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
        }
    }
}

pub async fn require_auth(
    State(auth): State<AuthState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if auth.loopback_only
        && (!valid_loopback_host(request.headers()) || !valid_loopback_origin(request.headers()))
    {
        return (StatusCode::FORBIDDEN, "loopback host/origin required").into_response();
    }
    if !auth.loopback_only && !valid_network_origin(request.headers()) {
        return (StatusCode::FORBIDDEN, "same-host origin required").into_response();
    }
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let valid = auth.anonymous
        || provided.is_some_and(|value| {
            if let Some(token) = value.strip_prefix("Bearer ") {
                return constant_time_equal(&Sha256::digest(token.as_bytes()), &auth.token_digest);
            }
            value
                .strip_prefix("Basic ")
                .zip(auth.password_digest.as_ref())
                .is_some_and(|(value, expected)| {
                    constant_time_equal(&Sha256::digest(value.as_bytes()), expected)
                })
        });
    if !valid {
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
}
