//! Amazon Bedrock Converse transport with bearer-token and SigV4 auth.

use std::{collections::BTreeMap, fmt::Write as _, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream;
use haarness_core::{
    CoreError, CoreResult, ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest,
    ProviderStream, STRUCTURED_OUTPUT_TOOL_NAME, structured_output_tool,
};
use haarness_protocol::{ModelInfo, ProviderInfo};
use hmac::{Hmac, Mac as _};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{CompatibleModelDefaults, CredentialStore, NativeInstance, ProviderHeaders};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
struct AwsCredentials {
    access_key: String,
    secret_key: SecretString,
    session_token: Option<SecretString>,
}

pub struct BedrockProvider {
    client: reqwest::Client,
    instance: NativeInstance,
    auth_store: Option<Arc<CredentialStore>>,
    region: String,
    bearer: Option<SecretString>,
    credentials: Option<AwsCredentials>,
    models: Vec<ModelInfo>,
}

impl BedrockProvider {
    pub fn from_environment(auth_store: Option<Arc<CredentialStore>>) -> Self {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".into());
        let base_url = std::env::var("AWS_BEDROCK_RUNTIME_ENDPOINT")
            .unwrap_or_else(|_| format!("https://bedrock-runtime.{region}.amazonaws.com"));
        let instance = NativeInstance {
            id: "amazon-bedrock".into(),
            name: "Amazon Bedrock".into(),
            base_url,
            api_key: None,
            models: vec![
                "us.anthropic.claude-sonnet-4-5-20250929-v1:0".into(),
                "amazon.nova-pro-v1:0".into(),
            ],
            discover_models: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: ProviderHeaders::default(),
        };
        Self::new_with_region(instance, auth_store, region)
    }

    pub fn new(instance: NativeInstance, auth_store: Option<Arc<CredentialStore>>) -> Self {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".into());
        Self::new_with_region(instance, auth_store, region)
    }

    fn new_with_region(
        instance: NativeInstance,
        auth_store: Option<Arc<CredentialStore>>,
        region: String,
    ) -> Self {
        let bearer = instance
            .api_key
            .clone()
            .or_else(|| std::env::var("AWS_BEARER_TOKEN_BEDROCK").ok())
            .filter(|value| !value.is_empty())
            .map(SecretString::from);
        let credentials = aws_credentials_from_environment();
        let ids = if instance.models.is_empty() {
            vec![
                "us.anthropic.claude-sonnet-4-5-20250929-v1:0".into(),
                "amazon.nova-pro-v1:0".into(),
            ]
        } else {
            instance.models.clone()
        };
        let models = ids
            .into_iter()
            .map(|id| {
                ModelInfo {
                    display_name: id.clone(),
                    id,
                    // Converse requests carry no thinking settings yet, so no
                    // reasoning level is offered that would be ignored.
                    reasoning: false,
                    reasoning_efforts: Vec::new(),
                    images: true,
                    video: false,
                    tools: true,
                    context_window: instance.model_defaults.context_window,
                    max_output_tokens: instance.model_defaults.max_output_tokens,
                    pricing: None,
                }
            })
            .collect();
        Self {
            client: reqwest::Client::new(),
            instance,
            auth_store,
            region,
            bearer,
            credentials,
            models,
        }
    }

    fn stored_bearer(&self) -> Option<String> {
        self.auth_store
            .as_ref()?
            .get(&self.instance.id)
            .ok()??
            .api_key()
            .map(str::to_owned)
    }

    fn authenticated(&self) -> bool {
        self.bearer.is_some() || self.credentials.is_some() || self.stored_bearer().is_some()
    }
}

#[async_trait]
impl ModelProvider for BedrockProvider {
    fn id(&self) -> &str {
        &self.instance.id
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.instance.id.clone(),
            name: self.instance.name.clone(),
            authenticated: self.authenticated(),
            models: self.models.clone(),
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let encoded_model = encode_path_segment(&request.model);
        let url = format!(
            "{}/model/{encoded_model}/converse",
            self.instance.base_url.trim_end_matches('/')
        );
        let body = bedrock_request(request)?;
        let payload = serde_json::to_vec(&body).map_err(|error| {
            CoreError::Provider(format!("cannot encode Bedrock request: {error}"))
        })?;
        let mut headers = self.instance.headers.map();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        if let Some(token) = self
            .bearer
            .as_ref()
            .map(|value| value.expose_secret().to_owned())
            .or_else(|| self.stored_bearer())
        {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(provider_error)?,
            );
        } else if let Some(credentials) = &self.credentials {
            sign_request(&url, &payload, &self.region, credentials, &mut headers)?;
        } else {
            return Err(CoreError::Provider(
                "Amazon Bedrock is not authenticated; set AWS_BEARER_TOKEN_BEDROCK, AWS credentials, or run `haarness auth login amazon-bedrock`".into(),
            ));
        }
        let response = self
            .client
            .post(url)
            .headers(headers)
            .body(payload)
            .send()
            .await
            .map_err(crate::sse::transport_error)?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            return Err(CoreError::Provider(format!(
                "Bedrock returned {status}: {}",
                detail.chars().take(1000).collect::<String>()
            )));
        }
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let response: Value = response.json().await.map_err(crate::sse::transport_error)?;
        let events = bedrock_events(&response);
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

fn bedrock_request(request: ProviderRequest) -> CoreResult<Value> {
    let mut messages = Vec::new();
    for message in request.messages {
        match message {
            ProviderMessage::Text { role, content } => {
                messages.push(json!({"role":role,"content":[{"text":content}]}));
            }
            ProviderMessage::Media {
                role,
                content,
                attachments,
            } => {
                let media = crate::media::load_all(&attachments)?;
                let mut blocks = vec![json!({"text":content})];
                for item in media {
                    if item.is_image() {
                        let format = item.mime_type.strip_prefix("image/").unwrap_or("png");
                        blocks.push(json!({"image":{"format":format,"source":{"bytes":item.base64}}}));
                    } else if item.is_pdf() {
                        blocks.push(json!({"document":{
                            "format":"pdf",
                            "name":bedrock_document_name(&item.filename),
                            "source":{"bytes":item.base64}
                        }}));
                    } else {
                        return Err(CoreError::Provider(
                            "Bedrock Converse accepts Haarness image and PDF attachments, not audio or video"
                                .into(),
                        ));
                    }
                }
                messages.push(json!({"role":role,"content":blocks}));
            }
            ProviderMessage::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => messages.push(json!({"role":"assistant","content":[{"toolUse":{"toolUseId":call_id,"name":name,"input":arguments}}]})),
            ProviderMessage::ToolResult {
                call_id,
                output,
                is_error,
                attachments,
                ..
            } => {
                let mut content = vec![json!({"text":output})];
                for item in crate::media::load_all(&attachments)? {
                    if item.is_image() {
                        let format = item.mime_type.strip_prefix("image/").unwrap_or("png");
                        content.push(json!({"image":{"format":format,"source":{"bytes":item.base64}}}));
                    } else if item.is_pdf() {
                        content.push(json!({"document":{
                            "format":"pdf",
                            "name":bedrock_document_name(&item.filename),
                            "source":{"bytes":item.base64}
                        }}));
                    } else {
                        return Err(CoreError::Provider(
                            "Bedrock Converse tool results support Haarness images and PDFs, not audio or video"
                                .into(),
                        ));
                    }
                }
                messages.push(json!({"role":"user","content":[{"toolResult":{"toolUseId":call_id,"content":content,"status":if is_error {"error"} else {"success"}}}]}));
            }
        }
    }
    let max_tokens = request
        .parameters
        .max_output_tokens
        .filter(|limit| *limit > 0)
        .unwrap_or(8192);
    let structured = request.output_schema;
    let forced = !crate::tool_schema::refuses_forced_tool_choice(&request.model);
    let mut requested_tools = request.tools;
    // Converse hands Claude the same tool schema as the Messages API.
    crate::tool_schema::retain_claude_compatible(&request.model, &mut requested_tools);
    if let Some(schema) = structured.as_ref() {
        requested_tools.retain(|tool| tool.name != STRUCTURED_OUTPUT_TOOL_NAME);
        requested_tools.push(structured_output_tool(schema));
    }
    let tools: Vec<_> = requested_tools
        .into_iter()
        .map(|tool| json!({"toolSpec":{"name":tool.name,"description":tool.description,"inputSchema":{"json":tool.input_schema}}}))
        .collect();
    let mut body = json!({
        "system":[{"text":request.system}],
        "messages":messages,
        // The Anthropic adapter's default; a configured limit wins.
        "inferenceConfig":{"maxTokens":max_tokens}
    });
    if !tools.is_empty() {
        body["toolConfig"] = json!({"tools":tools});
        if structured.is_some() && forced {
            body["toolConfig"]["toolChoice"] = json!({"tool":{"name":STRUCTURED_OUTPUT_TOOL_NAME}});
        }
    }
    Ok(body)
}

fn bedrock_document_name(filename: &str) -> String {
    let stem = filename.rsplit_once('.').map_or(filename, |(stem, _)| stem);
    let mut name = stem
        .chars()
        .map(|character| {
            if character.is_alphanumeric()
                || matches!(character, ' ' | '-' | '_' | '(' | ')' | '[' | ']')
            {
                character
            } else {
                '-'
            }
        })
        .take(100)
        .collect::<String>();
    if name.trim().is_empty() {
        name = "document".into();
    }
    name
}

fn bedrock_events(response: &Value) -> Vec<ProviderEvent> {
    let mut events = Vec::new();
    for block in response
        .pointer("/output/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            events.push(ProviderEvent::TextDelta(text.to_owned()));
        }
        if let Some(text) = block
            .pointer("/reasoningContent/reasoningText/text")
            .and_then(Value::as_str)
        {
            events.push(ProviderEvent::ReasoningDelta(text.to_owned()));
        }
        if let Some(call) = block.get("toolUse") {
            let name = call.get("name").and_then(Value::as_str).unwrap_or_default();
            let arguments = call.get("input").cloned().unwrap_or(Value::Null);
            if name == STRUCTURED_OUTPUT_TOOL_NAME {
                events.push(ProviderEvent::StructuredOutput(arguments));
            } else {
                events.push(ProviderEvent::ToolCall {
                    call_id: call
                        .get("toolUseId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: name.to_owned(),
                    arguments,
                    provider_metadata: Value::Null,
                });
            }
        }
    }
    if let Some(usage) = response.get("usage") {
        // Bedrock's inputTokens excludes cache reads and writes; the event's
        // input_tokens is the whole prompt.
        let read = number(usage, "cacheReadInputTokens");
        let write = number(usage, "cacheWriteInputTokens");
        events.push(ProviderEvent::Usage {
            input_tokens: number(usage, "inputTokens")
                .saturating_add(read)
                .saturating_add(write),
            output_tokens: number(usage, "outputTokens"),
            cache_read_tokens: read,
            cache_write_tokens: write,
        });
    }
    events.push(ProviderEvent::Finished {
        reason: response
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_owned(),
    });
    events
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn aws_credentials_from_environment() -> Option<AwsCredentials> {
    if let (Ok(access_key), Ok(secret_key)) = (
        std::env::var("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        return Some(AwsCredentials {
            access_key,
            secret_key: SecretString::from(secret_key),
            session_token: std::env::var("AWS_SESSION_TOKEN")
                .ok()
                .map(SecretString::from),
        });
    }
    let path = std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".aws/credentials"))
        })?;
    let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into());
    parse_aws_profile(&std::fs::read_to_string(path).ok()?, &profile)
}

fn parse_aws_profile(contents: &str, profile: &str) -> Option<AwsCredentials> {
    let mut in_profile = false;
    let mut values = BTreeMap::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            in_profile = section == profile || section == format!("profile {profile}");
            continue;
        }
        if in_profile && let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim(), value.trim().to_owned());
        }
    }
    Some(AwsCredentials {
        access_key: values.remove("aws_access_key_id")?,
        secret_key: SecretString::from(values.remove("aws_secret_access_key")?),
        session_token: values.remove("aws_session_token").map(SecretString::from),
    })
}

fn sign_request(
    endpoint: &str,
    payload: &[u8],
    region: &str,
    credentials: &AwsCredentials,
    headers: &mut reqwest::header::HeaderMap,
) -> CoreResult<()> {
    let url = url::Url::parse(endpoint).map_err(provider_error)?;
    let mut host = url
        .host_str()
        .ok_or_else(|| CoreError::Provider("Bedrock endpoint has no host".into()))?
        .to_owned();
    if let Some(port) = url.port() {
        let _ = write!(host, ":{port}");
    }
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let payload_hash = hex_sha256(payload);
    headers.insert("host", header(&host)?);
    headers.insert("x-amz-date", header(&amz_date)?);
    if let Some(token) = &credentials.session_token {
        headers.insert("x-amz-security-token", header(token.expose_secret())?);
    }
    let mut signed = vec![
        ("content-type", "application/json".to_owned()),
        ("host", host),
        ("x-amz-date", amz_date.clone()),
    ];
    if let Some(token) = &credentials.session_token {
        signed.push(("x-amz-security-token", token.expose_secret().to_owned()));
    }
    signed.sort_by_key(|(name, _)| *name);
    let canonical_headers = signed
        .iter()
        .fold(String::new(), |mut output, (name, value)| {
            let _ = writeln!(output, "{name}:{}", value.trim());
            output
        });
    let signed_headers = signed
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "POST\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        url.path(),
        url.query().unwrap_or_default()
    );
    let scope = format!("{date}/{region}/bedrock/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );
    let date_key = hmac(
        format!("AWS4{}", credentials.secret_key.expose_secret()).as_bytes(),
        date.as_bytes(),
    )?;
    let region_key = hmac(&date_key, region.as_bytes())?;
    let service_key = hmac(&region_key, b"bedrock")?;
    let signing_key = hmac(&service_key, b"aws4_request")?;
    let signature = hex_bytes(&hmac(&signing_key, string_to_sign.as_bytes())?);
    headers.insert(
        reqwest::header::AUTHORIZATION,
        header(&format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            credentials.access_key
        ))?,
    );
    Ok(())
}

fn hmac(key: &[u8], value: &[u8]) -> CoreResult<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|error| CoreError::Provider(format!("cannot initialize SigV4: {error}")))?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_sha256(value: &[u8]) -> String {
    hex_bytes(&Sha256::digest(value))
}

fn hex_bytes(value: &[u8]) -> String {
    value.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn header(value: &str) -> CoreResult<reqwest::header::HeaderValue> {
    reqwest::header::HeaderValue::from_str(value).map_err(provider_error)
}

fn encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn provider_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Provider(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
    use futures::StreamExt as _;
    use parking_lot::Mutex;
    use tokio::net::TcpListener;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<(HeaderMap, Value)>>>);

    async fn converse_fixture(
        State(capture): State<Capture>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        capture.0.lock().push((headers, body));
        Json(json!({
            "output":{"message":{"content":[{"text":"hello"}]}},
            "usage":{"inputTokens":2,"outputTokens":1},
            "stopReason":"end_turn"
        }))
    }

    #[test]
    fn profile_credentials_are_parsed_without_exposing_secrets() {
        let credentials = parse_aws_profile(
            "[default]\naws_access_key_id = key\naws_secret_access_key = secret\naws_session_token = session\n",
            "default",
        )
        .unwrap();
        assert_eq!(credentials.access_key, "key");
        assert_eq!(credentials.secret_key.expose_secret(), "secret");
    }

    #[test]
    fn converse_maps_tools_structured_output_and_usage() {
        let response = json!({
            "output":{"message":{"content":[
                {"reasoningContent":{"reasoningText":{"text":"thinking"}}},
                {"toolUse":{"toolUseId":"call-1","name":"StructuredOutput","input":{"ok":true}}}
            ]}},
            "usage":{"inputTokens":7,"outputTokens":3,"cacheReadInputTokens":2},
            "stopReason":"tool_use"
        });
        let events = bedrock_events(&response);
        assert!(matches!(&events[0], ProviderEvent::ReasoningDelta(value) if value == "thinking"));
        assert!(
            matches!(&events[1], ProviderEvent::StructuredOutput(value) if value == &json!({"ok":true}))
        );
        assert!(matches!(
            &events[2],
            // inputTokens 7 excludes the 2 cache reads: the whole prompt is 9.
            ProviderEvent::Usage {
                input_tokens: 9,
                output_tokens: 3,
                cache_read_tokens: 2,
                ..
            }
        ));
    }

    #[test]
    fn converse_serializes_pdf_input_and_pdf_tool_results() {
        let body = bedrock_request(ProviderRequest {
            model: "test-model".into(),
            system: "test".into(),
            messages: vec![
                ProviderMessage::Media {
                    role: "user".into(),
                    content: "Review this".into(),
                    attachments: vec![
                        "data:application/pdf;name=design%20notes.pdf;base64,JVBERi0xLjQK".into(),
                    ],
                },
                ProviderMessage::ToolCall {
                    call_id: "read-1".into(),
                    name: "read".into(),
                    arguments: json!({"path":"report.pdf"}),
                    provider_metadata: Value::Null,
                },
                ProviderMessage::ToolResult {
                    call_id: "read-1".into(),
                    name: "read".into(),
                    output: "PDF read successfully".into(),
                    is_error: false,
                    attachments: vec![
                        "data:application/pdf;name=report.pdf;base64,JVBERi0xLjQK".into(),
                    ],
                },
            ],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        })
        .unwrap();
        assert_eq!(
            body["messages"][0]["content"][1]["document"]["format"],
            "pdf"
        );
        assert_eq!(
            body["messages"][0]["content"][1]["document"]["name"],
            "design notes"
        );
        assert_eq!(
            body["messages"][2]["content"][0]["toolResult"]["content"][1]["document"]["format"],
            "pdf"
        );
    }

    #[test]
    fn structured_output_forces_the_tool_only_where_claude_accepts_it() {
        let choice = |model: &str| {
            bedrock_request(ProviderRequest {
                model: model.into(),
                system: "system".into(),
                messages: vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "count".into(),
                }],
                tools: Vec::new(),
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters::default(),
                output_schema: Some(json!({"type":"object","properties":{"n":{"type":"integer"}}})),
            })
            .unwrap()["toolConfig"]
                .get("toolChoice")
                .cloned()
        };
        assert!(choice("us.anthropic.claude-sonnet-4-6-v1:0").is_some());
        // Answers are no longer cut at 4,096 tokens; a configured limit wins.
        let limit = |max_output_tokens| {
            bedrock_request(ProviderRequest {
                model: "us.anthropic.claude-sonnet-5-5-v1:0".into(),
                system: "system".into(),
                messages: Vec::new(),
                tools: Vec::new(),
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters {
                    max_output_tokens,
                    ..haarness_core::RequestParameters::default()
                },
                output_schema: None,
            })
            .unwrap()["inferenceConfig"]["maxTokens"]
                .clone()
        };
        assert_eq!(limit(None), 8192);
        assert_eq!(limit(Some(32_000)), 32_000);
        // Opus/Sonnet 5.5 and Fable 5.1 answer 400 to a forced tool choice.
        assert!(choice("us.anthropic.claude-sonnet-5-5-v1:0").is_none());
        assert!(choice("global.anthropic.claude-opus-5-5").is_none());
    }

    #[test]
    fn claude_converse_requests_omit_tools_whose_root_schema_claude_rejects() {
        let tools = vec![
            haarness_core::ToolDefinition {
                name: "plain".into(),
                description: "plain".into(),
                input_schema: json!({"type":"object"}),
            },
            haarness_core::ToolDefinition {
                name: "mcp__fixture__either".into(),
                description: "either".into(),
                input_schema: json!({
                    "type":"object",
                    "oneOf":[{"required":["current"]},{"required":["all"]}]
                }),
            },
        ];
        let names = |model: &str| {
            let body = bedrock_request(ProviderRequest {
                model: model.into(),
                system: "system".into(),
                messages: vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "list".into(),
                }],
                tools: tools.clone(),
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters::default(),
                output_schema: None,
            })
            .unwrap();
            body["toolConfig"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["toolSpec"]["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("us.anthropic.claude-sonnet-4-6-v1:0"), ["plain"]);
        assert_eq!(
            names("amazon.nova-pro-v1:0"),
            ["plain", "mcp__fixture__either"]
        );
    }

    #[test]
    fn sigv4_signs_the_bedrock_service_and_session_token() {
        let credentials = AwsCredentials {
            access_key: "AKID".into(),
            secret_key: SecretString::from("secret"),
            session_token: Some(SecretString::from("session")),
        };
        let mut headers = reqwest::header::HeaderMap::new();
        sign_request(
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/test/converse",
            br"{}",
            "us-east-1",
            &credentials,
            &mut headers,
        )
        .unwrap();
        let authorization = headers[reqwest::header::AUTHORIZATION].to_str().unwrap();
        assert!(authorization.contains("/us-east-1/bedrock/aws4_request"));
        assert!(authorization.contains("x-amz-security-token"));
    }

    #[tokio::test]
    async fn converse_runs_end_to_end_with_bearer_auth() {
        let capture = Capture::default();
        let router = Router::new()
            .route("/model/test-model/converse", post(converse_fixture))
            .with_state(capture.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = BedrockProvider::new_with_region(
            NativeInstance {
                id: "amazon-bedrock".into(),
                name: "Amazon Bedrock".into(),
                base_url: format!("http://{address}"),
                api_key: Some("bedrock-token".into()),
                models: vec!["test-model".into()],
                discover_models: false,
                model_defaults: CompatibleModelDefaults::default(),
                assume_authenticated: false,
                headers: ProviderHeaders::default(),
            },
            None,
            "us-east-1".into(),
        );
        let request = ProviderRequest {
            model: "test-model".into(),
            system: "system".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let events = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(&events[0], Ok(ProviderEvent::TextDelta(text)) if text == "hello"));
        let requests = capture.0.lock();
        assert_eq!(requests[0].0["authorization"], "Bearer bedrock-token");
        assert_eq!(requests[0].1["messages"][0]["content"][0]["text"], "hi");
    }
}
