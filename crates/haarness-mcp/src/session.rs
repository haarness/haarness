//! Streamable HTTP servers that forget their sessions.
//!
//! A server that restarted, or ended a session, no longer knows the session
//! its client names. MCP has it answer 404, which rmcp takes for an expired
//! session: it opens a new session once and sends the request again. Servers
//! built on the TypeScript SDK's examples answer 400 instead: with a JSON-RPC
//! error that names no request (`Bad Request: No valid session ID
//! provided`), which rmcp let go since it answers no request it waits for,
//! so a tool call waited until the person stopped it; or with the same kind
//! of sentence as text (`Bad Request: Server not initialized`), which failed
//! every request until the server was reconnected by hand. [`SessionClient`]
//! reads both as the expired session they mean.

use std::{collections::HashMap, sync::Arc};

use http::{HeaderName, HeaderValue};
use rmcp::{
    model::{ClientJsonRpcMessage, RequestId, ServerJsonRpcMessage},
    transport::{
        common::client_side_sse::BoxedSseResponse,
        streamable_http_client::{
            StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
        },
    },
};

/// The HTTP client of a Streamable HTTP connection, which reads a server's
/// refusal of a session it forgot as rmcp's expired session, and an error
/// that names no request as the answer to the request it came back to.
#[derive(Clone)]
pub(super) struct SessionClient<C>(pub(super) C);

impl<C: StreamableHttpClient + Sync> StreamableHttpClient for SessionClient<C> {
    type Error = C::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let request = request_id(&message);
        let named = session_id.is_some();
        let answer = self
            .0
            .post_message(uri, message, session_id, auth_header, custom_headers)
            .await;
        read(answer, request, named)
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let request = request_id(&message);
        let named = session_id.is_some();
        let answer = self
            .0
            .post_message_with_max_sse_event_size(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .await;
        read(answer, request, named)
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        self.0
            .delete_session(uri, session_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxedSseResponse, StreamableHttpError<Self::Error>> {
        self.0
            .get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxedSseResponse, StreamableHttpError<Self::Error>> {
        self.0
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .await
    }
}

fn request_id(message: &ClientJsonRpcMessage) -> Option<RequestId> {
    match message {
        ClientJsonRpcMessage::Request(request) => Some(request.id.clone()),
        _ => None,
    }
}

/// The server's `answer` to the request `request` (none for a notification
/// or a response), sent with a session id when `named`. A refusal that is
/// about the session or about the server not being initialized, as JSON that
/// names no request or as a 400's text, means the server forgot the session:
/// it turned the request away before running it, so rmcp may open a new
/// session and send it again. Without a session id the request was the
/// first of one, and such a refusal stays what it is. Any other error that
/// names no request is this request's: a server that cannot read a
/// request's id answers it without one.
fn read<E: std::error::Error + Send + Sync + 'static>(
    answer: Result<StreamableHttpPostResponse, StreamableHttpError<E>>,
    request: Option<RequestId>,
    named: bool,
) -> Result<StreamableHttpPostResponse, StreamableHttpError<E>> {
    match answer {
        Ok(StreamableHttpPostResponse::Json(ServerJsonRpcMessage::Error(mut error), session))
            if error.id.is_none() =>
        {
            if named && forgotten(&error.error.message) {
                return Err(StreamableHttpError::SessionExpired);
            }
            error.id = request;
            Ok(StreamableHttpPostResponse::Json(
                ServerJsonRpcMessage::Error(error),
                session,
            ))
        }
        Err(StreamableHttpError::UnexpectedServerResponse(text))
            if named && text.strip_prefix("HTTP 400 ").is_some_and(forgotten) =>
        {
            Err(StreamableHttpError::SessionExpired)
        }
        answer => answer,
    }
}

/// Whether a refusal says the server does not know the session or was
/// never initialized for it.
fn forgotten(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("session") || message.contains("not initialized")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{ErrorCode, ErrorData, JsonRpcError, JsonRpcVersion2_0, NumberOrString};

    type Answer = Result<StreamableHttpPostResponse, StreamableHttpError<std::io::Error>>;

    fn refusal(message: &'static str, id: Option<RequestId>) -> StreamableHttpPostResponse {
        StreamableHttpPostResponse::Json(
            ServerJsonRpcMessage::Error(JsonRpcError {
                jsonrpc: JsonRpcVersion2_0,
                id,
                error: ErrorData::new(ErrorCode(-32000), message, None),
            }),
            None,
        )
    }

    fn expired(answer: &Answer) -> bool {
        matches!(answer, Err(StreamableHttpError::SessionExpired))
    }

    /// The request an error answer names as the one it answers.
    fn answering(answer: &Answer) -> Option<RequestId> {
        match answer {
            Ok(StreamableHttpPostResponse::Json(ServerJsonRpcMessage::Error(error), _)) => {
                error.id.clone()
            }
            other => panic!("not an error answer: {other:?}"),
        }
    }

    #[test]
    fn a_refused_session_reads_as_expired_only_when_the_request_named_one() {
        let id = RequestId::from(NumberOrString::Number(7));
        let forgot = "Bad Request: No valid session ID provided";
        assert!(expired(&read(
            Ok(refusal(forgot, None)),
            Some(id.clone()),
            true
        )));
        let text = || {
            Err(StreamableHttpError::UnexpectedServerResponse(
                "HTTP 400 Bad Request: Bad Request: Server not initialized".into(),
            ))
        };
        assert!(expired(&read(text(), Some(id.clone()), true)));
        // The first request of a session: what it is refused for stays.
        assert_eq!(
            answering(&read(Ok(refusal(forgot, None)), Some(id.clone()), false)),
            Some(id.clone())
        );
        assert!(!expired(&read(text(), Some(id.clone()), false)));
        // A 500 about a session is the server failing, not forgetting.
        let failed: Answer = Err(StreamableHttpError::UnexpectedServerResponse(
            "HTTP 500 Internal Server Error: session store unavailable".into(),
        ));
        assert!(!expired(&read(failed, Some(id.clone()), true)));
        // Any other refusal that names no request answers this one.
        assert_eq!(
            answering(&read(
                Ok(refusal("Invalid params: city is required", None)),
                Some(id.clone()),
                true
            )),
            Some(id.clone())
        );
        // One that names a request is left as it is.
        let other = RequestId::from(NumberOrString::Number(3));
        assert_eq!(
            answering(&read(
                Ok(refusal("Invalid params", Some(other.clone()))),
                Some(id),
                true
            )),
            Some(other)
        );
    }
}
