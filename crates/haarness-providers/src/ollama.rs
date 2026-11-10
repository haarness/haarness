//! Ollama's own chat API, `/api/chat`.
//!
//! Ollama's OpenAI-style endpoint runs every model with the server's default
//! context, which Ollama sets by the computer's GPU memory: 4K tokens under
//! 24 GiB, as on most laptops and gaming PCs. A longer prompt is cut from the
//! front without a word, so Haarness's instructions and tools went first and
//! the model answered something it was never asked, or printed a tool call as
//! text. That endpoint has no way to ask for more. Ollama's own API takes the
//! context to run the model with (`options.num_ctx`) and, with `truncate`
//! off, refuses a prompt that still doesn't fit, which compacting the chat
//! answers. Haarness builds its usual Chat Completions body and turns it into
//! this API's form here; the answer's lines come back as Chat Completions
//! chunks, so the rest of the provider reads both alike.

use crate::sse::JsonStream;
use async_stream::try_stream;
use futures::StreamExt;
use haarness_core::CoreError;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

/// The context Haarness runs an Ollama model with, unless the model is
/// shorter or the person chose more. Haarness's instructions and tools take
/// about 13K tokens with a small model; this leaves the rest for the chat,
/// and its memory still fits beside a 7B or 8B model on an 8 GB GPU.
pub(crate) const CONTEXT: u64 = 32_768;

/// Where Ollama listens: `OLLAMA_HOST` as Ollama itself reads it (a host, a
/// port or both, a scheme optional), else `127.0.0.1:11434`. A server told
/// to listen on every address (`0.0.0.0`, `::`) is reached on this computer,
/// and `localhost` as `127.0.0.1`, where Ollama listens for it: Windows
/// tries `::1` first, and a refused connection there takes it seconds.
pub(crate) fn root_url(host: Option<&str>) -> String {
    const DEFAULT: &str = "http://127.0.0.1:11434";
    let Some(host) = host.map(str::trim).filter(|host| !host.is_empty()) else {
        return DEFAULT.into();
    };
    let (scheme, rest) = host
        .split_once("://")
        .map_or(("http", host), |(scheme, rest)| (scheme, rest));
    let rest = rest.trim_end_matches('/');
    // `[::1]:11434`, `[::]`, `::1`, `host:port`, `host`, `:port`.
    let (name, port) = if let Some(bracketed) = rest.strip_prefix('[') {
        match bracketed.split_once(']') {
            Some((name, tail)) => (name, tail.strip_prefix(':')),
            None => return DEFAULT.into(),
        }
    } else if rest.matches(':').count() > 1 {
        (rest, None)
    } else {
        match rest.rsplit_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (rest, None),
        }
    };
    let port = port
        .filter(|port| !port.is_empty())
        .unwrap_or(if scheme == "https" { "443" } else { "11434" });
    let name = match name {
        "" | "0.0.0.0" => "127.0.0.1".to_owned(),
        name if name.eq_ignore_ascii_case("localhost") => "127.0.0.1".to_owned(),
        "::" => "[::1]".to_owned(),
        name if name.contains(':') => format!("[{name}]"),
        name => name.to_owned(),
    };
    format!("{scheme}://{name}:{port}")
}

/// The context the person chose for Ollama on this computer, as Ollama
/// reads it: `OLLAMA_CONTEXT_LENGTH`.
pub(crate) fn chosen_context() -> Option<u64> {
    std::env::var("OLLAMA_CONTEXT_LENGTH")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|tokens| *tokens > 0)
}

/// The longest context a model takes, from `/api/show`:
/// `<architecture>.context_length` (`qwen3.context_length`), never the
/// window a model was trained with before it was stretched
/// (`…rope.scaling.original_context_length`).
pub(crate) fn longest_context(details: &Value) -> Option<u64> {
    let info = details["model_info"].as_object()?;
    let exact = info
        .get("general.architecture")
        .and_then(Value::as_str)
        .and_then(|architecture| info.get(&format!("{architecture}.context_length")));
    exact
        .or_else(|| {
            info.iter()
                .find(|(key, _)| {
                    key.ends_with(".context_length") && !key.contains("original_context_length")
                })
                .map(|(_, value)| value)
        })
        .and_then(Value::as_u64)
        .filter(|tokens| *tokens > 0)
}

/// The `num_ctx` a model's own settings (its Modelfile) give it, from
/// `/api/show`'s `parameters` text (`num_ctx    32768`).
pub(crate) fn modelfile_context(details: &Value) -> Option<u64> {
    details["parameters"].as_str()?.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next() == Some("num_ctx"))
            .then(|| words.next()?.parse().ok())
            .flatten()
    })
}

/// The context Haarness runs a model with, which is also the window it
/// compacts the chat against: [`CONTEXT`], or what the person chose for
/// Ollama or the model when that is more, and never more than the model
/// takes. A smaller choice is passed over: below this, the instructions and
/// tools leave too little room for the chat, and Ollama would drop the
/// start of every prompt.
pub(crate) fn context_window(longest: Option<u64>, chosen: &[Option<u64>]) -> u64 {
    let wanted = chosen
        .iter()
        .flatten()
        .copied()
        .filter(|tokens| *tokens >= CONTEXT)
        .max()
        .unwrap_or(CONTEXT);
    longest.map_or(wanted, |longest| wanted.min(longest))
}

/// The Chat Completions fields [`chat_body`] puts into `/api/chat`'s own.
const TRANSLATED: [&str; 14] = [
    "model",
    "messages",
    "stream",
    "stream_options",
    "max_tokens",
    "temperature",
    "top_p",
    "top_k",
    "seed",
    "stop",
    "tools",
    "tool_choice",
    "reasoning_effort",
    "options",
];

/// `/api/chat`'s body for a Chat Completions `body`, run with `num_ctx`.
pub(crate) fn chat_body(body: &Value, num_ctx: u64) -> Value {
    let mut names = HashMap::new();
    let mut messages = Vec::new();
    for message in body["messages"].as_array().into_iter().flatten() {
        let mut out = Map::new();
        let role = message["role"].as_str().unwrap_or("user");
        out.insert("role".into(), json!(role));
        let (content, images) = content_and_images(&message["content"]);
        out.insert("content".into(), json!(content));
        if !images.is_empty() {
            out.insert("images".into(), json!(images));
        }
        // The reasoning goes back as Ollama sent it, under its own name.
        if let Some(thinking) = ["reasoning", "reasoning_content", "thinking"]
            .iter()
            .find_map(|field| message[*field].as_str().filter(|text| !text.is_empty()))
        {
            out.insert("thinking".into(), json!(thinking));
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            let calls: Vec<Value> = calls
                .iter()
                .map(|call| {
                    let name = call["function"]["name"].as_str().unwrap_or_default();
                    if let Some(id) = call["id"].as_str() {
                        names.insert(id.to_owned(), name.to_owned());
                    }
                    // Ollama takes a call's arguments as an object.
                    let arguments = match &call["function"]["arguments"] {
                        Value::String(text) => serde_json::from_str::<Value>(text)
                            .ok()
                            .filter(Value::is_object)
                            .unwrap_or_else(|| json!({})),
                        Value::Object(object) => Value::Object(object.clone()),
                        _ => json!({}),
                    };
                    let mut out = json!({"function": {"name": name, "arguments": arguments}});
                    if let Some(id) = call["id"].as_str() {
                        out["id"] = json!(id);
                    }
                    out
                })
                .collect();
            out.insert("tool_calls".into(), json!(calls));
        }
        if role == "tool"
            && let Some(id) = message["tool_call_id"].as_str()
        {
            out.insert("tool_call_id".into(), json!(id));
            if let Some(name) = names.get(id) {
                out.insert("tool_name".into(), json!(name));
            }
        }
        messages.push(Value::Object(out));
    }
    let mut options = Map::new();
    options.insert("num_ctx".into(), json!(num_ctx));
    if let Some(limit) = body["max_tokens"].as_u64() {
        options.insert("num_predict".into(), json!(limit));
    }
    for key in ["temperature", "top_p", "top_k", "seed", "stop"] {
        if let Some(value) = body.get(key).filter(|value| !value.is_null()) {
            options.insert(key.into(), value.clone());
        }
    }
    // Options a request hook set itself win, `num_ctx` too.
    if let Some(chosen) = body["options"].as_object() {
        options.extend(chosen.clone());
    }
    let mut native = json!({
        "model": body["model"],
        "messages": messages,
        "stream": true,
        "options": options,
        // A prompt longer than the context is refused rather than cut from
        // the front, so the chat is compacted instead of the model reading
        // half of it.
        "truncate": false,
        // A reply that fills the context ends there (as `length`) rather
        // than going on with the front of the chat dropped: Ollama sets no
        // output limit, so a model caught in a loop (Qwen3 8B thought for
        // 15,000 tokens and went on) wrote until someone stopped it, its
        // instructions long gone.
        "shift": false,
    });
    if let Some(tools) = body["tools"].as_array().filter(|tools| !tools.is_empty()) {
        native["tools"] = json!(tools);
    }
    if let Some(effort) = body["reasoning_effort"].as_str() {
        native["think"] = match effort {
            "none" => json!(false),
            "minimal" | "low" => json!("low"),
            "medium" => json!("medium"),
            _ => json!("high"),
        };
    }
    // Anything else a request hook added goes along as it is.
    if let Some(body) = body.as_object() {
        for (key, value) in body {
            if !TRANSLATED.contains(&key.as_str()) && native.get(key).is_none() {
                native[key.as_str()] = value.clone();
            }
        }
    }
    native
}

/// A Chat Completions message's content as Ollama takes it: its text, and
/// its pictures as bare base64.
fn content_and_images(content: &Value) -> (String, Vec<String>) {
    let Some(parts) = content.as_array() else {
        return (content.as_str().unwrap_or_default().to_owned(), Vec::new());
    };
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        match part["type"].as_str() {
            Some("text") => {
                if let Some(text) = part["text"].as_str() {
                    texts.push(text.to_owned());
                }
            }
            Some("image_url") => {
                if let Some(data) = part["image_url"]["url"]
                    .as_str()
                    .and_then(|url| url.split_once(";base64,"))
                    .map(|(_, data)| data)
                {
                    images.push(data.to_owned());
                }
            }
            Some("file") => {
                let name = part["file"]["filename"].as_str().unwrap_or("A file");
                texts.push(format!(
                    "[{name} was attached, but Ollama takes only text and pictures.]"
                ));
            }
            _ => {}
        }
    }
    (texts.join("\n\n"), images)
}

/// Ollama's answer, one JSON value a line, as Chat Completions chunks: its
/// text, its thinking (as `reasoning`, the field its OpenAI-style endpoint
/// uses), each tool call whole, and on the last line why it stopped and its
/// usage. An error Ollama reports in the stream ends it.
///
/// Some small models write a call to one of `tools` as the whole of their
/// reply instead of in their template's form (Qwen2.5 Coder answers
/// `{"name": "read", "arguments": {…}}` with no `<tool_call>` around it), so
/// Ollama passes it on as text and nothing runs. A reply that starts like
/// one is held until it ends, and is the call when it is nothing else.
pub(crate) fn chat_chunks(mut lines: JsonStream, tools: HashSet<String>) -> JsonStream {
    Box::pin(try_stream! {
        let mut calls = 0_usize;
        // Text held back while it may be a call written out.
        let mut held: Option<String> = None;
        let mut started = false;
        while let Some(line) = lines.next().await {
            let line = line?;
            if let Some(error) = line.get("error").filter(|error| !error.is_null()) {
                let message = error
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| error["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| error.to_string());
                Err(CoreError::Provider(message))?;
            }
            let message = &line["message"];
            let done = line["done"].as_bool() == Some(true);
            let mut delta = Map::new();
            if let Some(text) = message["content"].as_str().filter(|text| !text.is_empty()) {
                if !started && !text.trim().is_empty() {
                    started = true;
                    if !tools.is_empty() && calls == 0 && may_be_written_call(text) {
                        held = Some(String::new());
                    }
                }
                match held.as_mut() {
                    Some(held) => held.push_str(text),
                    None => {
                        delta.insert("content".into(), json!(text));
                    }
                }
            }
            if let Some(thinking) = message["thinking"].as_str().filter(|text| !text.is_empty()) {
                delta.insert("reasoning".into(), json!(thinking));
            }
            let mut tool_calls: Vec<Value> = message["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|call| {
                    let index = calls;
                    calls += 1;
                    let arguments = match &call["function"]["arguments"] {
                        Value::String(text) => text.clone(),
                        Value::Null => "{}".into(),
                        arguments => arguments.to_string(),
                    };
                    let id = call["id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .map_or_else(call_id, str::to_owned);
                    tool_call(index, &id, &call["function"]["name"], arguments)
                })
                .collect();
            if done && let Some(text) = held.take() {
                match written_calls(&text, &tools).filter(|_| calls == 0) {
                    Some(written) => {
                        tracing::info!(calls = written.len(), "reading a tool call an Ollama model wrote as text");
                        for (name, arguments) in written {
                            let index = calls;
                            calls += 1;
                            tool_calls.push(tool_call(index, &call_id(), &json!(name), arguments.to_string()));
                        }
                    }
                    None => {
                        delta.insert("content".into(), json!(text));
                    }
                }
            }
            if !tool_calls.is_empty() {
                delta.insert("tool_calls".into(), json!(tool_calls));
            }
            let mut chunk = json!({"choices": [{"index": 0, "delta": delta}]});
            if done {
                let reason = if calls > 0 {
                    "tool_calls"
                } else {
                    match line["done_reason"].as_str() {
                        Some("length") => "length",
                        _ => "stop",
                    }
                };
                chunk["choices"][0]["finish_reason"] = json!(reason);
                chunk["usage"] = json!({
                    "prompt_tokens": line["prompt_eval_count"].as_u64().unwrap_or(0),
                    "completion_tokens": line["eval_count"].as_u64().unwrap_or(0),
                    "prompt_tokens_details": {
                        "cached_tokens": line["prompt_eval_cached_count"].as_u64().unwrap_or(0),
                    },
                });
            }
            yield chunk;
        }
        // Ended without its last line: what was held is text after all.
        if let Some(text) = held.take().filter(|text| !text.is_empty()) {
            yield json!({"choices": [{"index": 0, "delta": {"content": text}}]});
        }
    })
}

/// Ollama's error for a reply it ended, said as a tool call the model wrote
/// that Ollama could not read ([`haarness_core::UNREADABLE_TOOL_CALL`]) when
/// it is one, for the model to write again; any other error as it was.
///
/// Ollama reads the calls of gpt-oss, Qwen 3.5, Qwen3-Coder, GLM, Gemma 4 and
/// others with a parser of its own, and ends the reply with an error when one
/// doesn't parse: in the stream once the reply has begun (gpt-oss reasons
/// first), as a 500 before. gpt-oss's call to `write` that runs out of room
/// in the context ends `error parsing tool call: raw='{"content":"…',
/// err=unexpected end of JSON input`, the file so far quoted whole: the
/// model's own words, which are left out.
pub(crate) fn readable_error(error: CoreError) -> CoreError {
    let CoreError::Provider(message) = &error else {
        return error;
    };
    if message.starts_with(haarness_core::UNREADABLE_TOOL_CALL) {
        return error;
    }
    // A refusal before the reply began: "HTTP 500 Internal Server Error: …".
    let words = message
        .strip_prefix("HTTP 5")
        .and_then(|rest| rest.split_once(": "))
        .map_or(message.as_str(), |(_, words)| words);
    if !unreadable_call(words) {
        return error;
    }
    // gpt-oss's: the call as written, then why it doesn't parse.
    let reason = match words.split_once(" raw='") {
        Some((what, rest)) => match rest.rsplit_once("', err=") {
            Some((_, why)) => format!("{what} {why}"),
            None => what.to_owned(),
        },
        None => words.to_owned(),
    };
    CoreError::Provider(format!("{}{reason}", haarness_core::UNREADABLE_TOOL_CALL))
}

/// Whether `words` are an Ollama parser's for a model's tool call it could
/// not read (Ollama 0.40): gpt-oss's `error parsing tool call`, Qwen's
/// `failed to parse JSON` and Qwen3-Coder's XML errors, GLM's and Gemma 4's
/// calls, llama-server's `invalid tool call arguments`, OLMo 3's `function
/// call`, and Ministral's `tool '…' not found`.
fn unreadable_call(words: &str) -> bool {
    let lower = words.to_ascii_lowercase();
    [
        "tool call",
        "failed to parse json",
        "xml syntax error",
        "expected element type",
        "failed to parse xml",
        "is not declared",
        "required argument",
        "mismatched arg_key",
        "empty function name",
        "empty argument name",
        "function call",
        "failed to parse arguments",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
        || (lower.starts_with("tool '") && lower.ends_with("' not found"))
}

fn tool_call(index: usize, id: &str, name: &Value, arguments: String) -> Value {
    json!({
        "index": index,
        "id": id,
        "type": "function",
        "function": {"name": name, "arguments": arguments},
    })
}

/// Whether a reply's first text could open a call written out: a JSON
/// object, a code fence or a `<tool_call>` tag.
fn may_be_written_call(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with('{') || text.starts_with('`') || text.starts_with('<')
}

/// The calls a reply writes out, when it is nothing but calls to `tools`:
/// one or more `{"name": …, "arguments": {…}}` objects (`parameters` in
/// Llama's form), each maybe in `<tool_call>` tags, the lot maybe in a
/// code fence. `None` for anything else, which stays text.
fn written_calls(text: &str, tools: &HashSet<String>) -> Option<Vec<(String, Value)>> {
    let mut text = text.trim();
    if let Some(fenced) = text.strip_prefix("```") {
        let fenced = fenced.strip_suffix("```")?;
        // The fence's language, `json`, up to the line's end.
        text = fenced
            .split_once('\n')
            .map_or(fenced, |(_, body)| body)
            .trim();
    }
    let text = text.replace("<tool_call>", "").replace("</tool_call>", "");
    let mut written = Vec::new();
    for value in serde_json::Deserializer::from_str(&text).into_iter::<Value>() {
        let value = value.ok()?;
        let name = value["name"]
            .as_str()
            .filter(|name| tools.contains(*name))?;
        let arguments = match value.get("arguments").or_else(|| value.get("parameters"))? {
            Value::Object(arguments) => Value::Object(arguments.clone()),
            Value::String(text) => serde_json::from_str::<Value>(text)
                .ok()
                .filter(Value::is_object)?,
            _ => return None,
        };
        let extra = value.as_object()?.keys().any(|key| {
            !matches!(
                key.as_str(),
                "name" | "arguments" | "parameters" | "type" | "id"
            )
        });
        if extra {
            return None;
        }
        written.push((name.to_owned(), arguments));
    }
    (!written.is_empty()).then_some(written)
}

/// An id for a call Ollama sent without one.
pub(crate) fn call_id() -> String {
    use rand::RngCore as _;
    let mut bytes = [0_u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().fold(String::from("call_"), |mut id, byte| {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
        id
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CONTEXT, chat_body, chat_chunks, context_window, longest_context, modelfile_context,
        root_url,
    };
    use futures::StreamExt;
    use serde_json::{Value, json};
    use std::collections::HashSet;

    #[test]
    fn ollama_host_is_read_as_ollama_reads_it() {
        assert_eq!(root_url(None), "http://127.0.0.1:11434");
        assert_eq!(root_url(Some("")), "http://127.0.0.1:11434");
        assert_eq!(root_url(Some("0.0.0.0")), "http://127.0.0.1:11434");
        assert_eq!(root_url(Some("0.0.0.0:11435")), "http://127.0.0.1:11435");
        assert_eq!(root_url(Some(":11500")), "http://127.0.0.1:11500");
        assert_eq!(root_url(Some("localhost:8080")), "http://127.0.0.1:8080");
        assert_eq!(root_url(Some("http://LOCALHOST")), "http://127.0.0.1:11434");
        assert_eq!(
            root_url(Some("http://gpu-box.lan:11434/")),
            "http://gpu-box.lan:11434"
        );
        assert_eq!(
            root_url(Some("https://ollama.example.com")),
            "https://ollama.example.com:443"
        );
        assert_eq!(root_url(Some("[::]:11434")), "http://[::1]:11434");
        assert_eq!(root_url(Some("::")), "http://[::1]:11434");
        assert_eq!(root_url(Some("[::1]")), "http://[::1]:11434");
        assert_eq!(root_url(Some("192.168.1.20")), "http://192.168.1.20:11434");
    }

    #[test]
    fn a_model_runs_with_room_for_the_instructions_and_tools() {
        // Ollama's own default on a small GPU is 4K: never taken.
        assert_eq!(context_window(Some(131_072), &[]), CONTEXT);
        assert_eq!(context_window(None, &[]), CONTEXT);
        // A model shorter than that runs with all it has.
        assert_eq!(context_window(Some(8_192), &[]), 8_192);
        // More, when the person chose it for Ollama or the model.
        assert_eq!(context_window(Some(131_072), &[Some(65_536), None]), 65_536);
        assert_eq!(
            context_window(Some(131_072), &[Some(65_536), Some(98_304)]),
            98_304
        );
        assert_eq!(context_window(Some(40_960), &[Some(65_536)]), 40_960);
        // Less is passed over.
        assert_eq!(context_window(Some(131_072), &[Some(4_096)]), CONTEXT);
    }

    #[test]
    fn show_says_how_long_a_model_reads_and_what_it_was_set_to() {
        // gpt-oss was trained at 4K, then stretched to 128K.
        let oss = json!({"model_info": {
            "general.architecture": "gptoss",
            "gptoss.context_length": 131_072,
            "gptoss.rope.scaling.original_context_length": 4_096,
        }});
        assert_eq!(longest_context(&oss), Some(131_072));
        let unnamed = json!({"model_info": {
            "qwen3.rope.scaling.original_context_length": 32_768,
            "qwen3.context_length": 131_072,
        }});
        assert_eq!(longest_context(&unnamed), Some(131_072));
        assert_eq!(longest_context(&json!({})), None);
        let tuned = json!({"parameters": "stop                           \"<|im_start|>\"\nnum_ctx                        65536\ntemperature                    0.7"});
        assert_eq!(modelfile_context(&tuned), Some(65_536));
        assert_eq!(
            modelfile_context(&json!({"parameters": "temperature 1"})),
            None
        );
        assert_eq!(modelfile_context(&json!({})), None);
    }

    #[test]
    fn a_chat_completions_body_becomes_ollamas_own() {
        let body = json!({
            "model": "qwen3:8b",
            "stream": true,
            "stream_options": {"include_usage": true},
            "max_tokens": 2_048,
            "temperature": 0.2,
            "reasoning_effort": "none",
            "tool_choice": "required",
            "tools": [{"type": "function", "function": {"name": "read", "description": "Read a file", "parameters": {"type": "object"}}}],
            "messages": [
                {"role": "system", "content": "You are Haarness."},
                {"role": "user", "content": [
                    {"type": "text", "text": "What is in this picture?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}},
                ]},
                {"role": "assistant", "content": null, "reasoning": "I should read it.", "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"calc.py\"}"}},
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "def add(a, b): ..."},
                {"role": "assistant", "content": "It adds."},
            ],
        });
        let native = chat_body(&body, 32_768);
        assert_eq!(
            native,
            json!({
                "model": "qwen3:8b",
                "stream": true,
                "truncate": false,
                "shift": false,
                "think": false,
                "options": {"num_ctx": 32_768, "num_predict": 2_048, "temperature": 0.2},
                "tools": [{"type": "function", "function": {"name": "read", "description": "Read a file", "parameters": {"type": "object"}}}],
                "messages": [
                    {"role": "system", "content": "You are Haarness."},
                    {"role": "user", "content": "What is in this picture?", "images": ["iVBORw0KGgo="]},
                    {"role": "assistant", "content": "", "thinking": "I should read it.", "tool_calls": [
                        {"id": "call_1", "function": {"name": "read", "arguments": {"path": "calc.py"}}},
                    ]},
                    {"role": "tool", "content": "def add(a, b): ...", "tool_call_id": "call_1", "tool_name": "read"},
                    {"role": "assistant", "content": "It adds."},
                ],
            })
        );
        // A level Ollama names its own way; a hook's own options win.
        let mut leveled = body.clone();
        leveled["reasoning_effort"] = json!("xhigh");
        leveled["options"] = json!({"num_ctx": 65_536});
        leveled["keep_alive"] = json!("30m");
        let native = chat_body(&leveled, 32_768);
        assert_eq!(native["think"], "high");
        assert_eq!(native["options"]["num_ctx"], 65_536);
        assert_eq!(native["keep_alive"], "30m");
        leveled["reasoning_effort"] = json!("minimal");
        assert_eq!(chat_body(&leveled, 32_768)["think"], "low");
    }

    async fn chunks(lines: Vec<Value>) -> Vec<Value> {
        let lines = futures::stream::iter(lines.into_iter().map(Ok));
        let tools = ["read".to_owned(), "edit".to_owned(), "list".to_owned()].into();
        chat_chunks(Box::pin(lines), tools)
            .map(|chunk| chunk.expect("a chunk"))
            .collect()
            .await
    }

    #[tokio::test]
    async fn ollamas_lines_read_as_chat_completions_chunks() {
        let read = chunks(vec![
            json!({"message": {"role": "assistant", "content": "", "thinking": "Read it."}, "done": false}),
            json!({"message": {"role": "assistant", "content": "Reading."}, "done": false}),
            json!({"message": {"role": "assistant", "content": "", "tool_calls": [
                {"id": "call_x", "function": {"index": 0, "name": "read", "arguments": {"path": "calc.py"}}},
                {"function": {"name": "list", "arguments": {}}},
            ]}, "done": true, "done_reason": "stop", "prompt_eval_count": 12_597, "prompt_eval_cached_count": 12_000, "eval_count": 35}),
        ])
        .await;
        assert_eq!(
            read[0]["choices"][0]["delta"],
            json!({"reasoning": "Read it."})
        );
        assert_eq!(
            read[1]["choices"][0]["delta"],
            json!({"content": "Reading."})
        );
        let last = &read[2];
        let calls = &last["choices"][0]["delta"]["tool_calls"];
        assert_eq!(calls[0]["index"], 0);
        assert_eq!(calls[0]["id"], "call_x");
        assert_eq!(calls[0]["function"]["name"], "read");
        assert_eq!(calls[0]["function"]["arguments"], "{\"path\":\"calc.py\"}");
        assert_eq!(calls[1]["index"], 1);
        assert!(
            calls[1]["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("call_"))
        );
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            last["usage"],
            json!({"prompt_tokens": 12_597, "completion_tokens": 35, "prompt_tokens_details": {"cached_tokens": 12_000}})
        );
        let cut = chunks(vec![
            json!({"message": {"role": "assistant", "content": "a"}, "done": true, "done_reason": "length", "prompt_eval_count": 3, "eval_count": 1}),
        ])
        .await;
        assert_eq!(cut[0]["choices"][0]["finish_reason"], "length");
    }

    #[tokio::test]
    async fn an_error_in_the_stream_ends_it_in_ollamas_words() {
        let lines = futures::stream::iter(vec![
            Ok(json!({"message": {"role": "assistant", "content": "x"}, "done": false})),
            Ok(
                json!({"error": "error parsing tool call: raw='{\"name\"', err=unexpected end of JSON input"}),
            ),
        ]);
        let mut stream = chat_chunks(Box::pin(lines), HashSet::new());
        assert!(stream.next().await.is_some_and(|chunk| chunk.is_ok()));
        let error = stream
            .next()
            .await
            .expect("an error")
            .expect_err("an error");
        assert!(
            error.to_string().contains("error parsing tool call"),
            "{error}"
        );
    }

    #[test]
    fn a_tool_call_ollama_could_not_read_is_said_as_one() {
        use haarness_core::{CoreError, UNREADABLE_TOOL_CALL};
        let read = |message: &str| match super::readable_error(CoreError::Provider(message.into()))
        {
            CoreError::Provider(words) => words,
            other => panic!("{other:?}"),
        };
        // gpt-oss on Ollama 0.40.6, its call to `write` cut off where the
        // reply filled the context: the file so far is left out.
        let file = "#!/usr/bin/env python3\\n\\\"\\\"\\\"Snake game', err= for the terminal";
        assert_eq!(
            read(&format!(
                "error parsing tool call: raw='{{\"content\":\"{file}', err=unexpected end of JSON input"
            )),
            format!("{UNREADABLE_TOOL_CALL}error parsing tool call: unexpected end of JSON input")
        );
        // The same refused before the reply began, as a 500.
        assert_eq!(
            read(
                "HTTP 500 Internal Server Error: error parsing tool call: raw='{\"path\": calc.py}', err=invalid character 'c' looking for beginning of value"
            ),
            format!(
                "{UNREADABLE_TOOL_CALL}error parsing tool call: invalid character 'c' looking for beginning of value"
            )
        );
        // Qwen 3.5's and Qwen3-Coder's parsers, Ministral's, llama-server's.
        for words in [
            "failed to parse JSON: invalid character '}' after object key",
            "XML syntax error on line 4: unexpected EOF",
            "tool 'deploy' not found",
            "llama-server returned invalid tool call arguments for \"read\": unexpected end of JSON input",
        ] {
            assert_eq!(read(words), format!("{UNREADABLE_TOOL_CALL}{words}"));
        }
        // Said once, however often it is read.
        let once = read("failed to parse JSON: x");
        assert_eq!(read(&once), once);
        // Anything else stays in Ollama's words.
        for words in [
            "model runner has unexpectedly stopped, this may be due to resource limitations or an internal error, check ollama server logs for details",
            "HTTP 400 Bad Request: request (40211 tokens) exceeds the available context size (32768 tokens), try increasing it",
            "HTTP 404 Not Found: model 'qwen3:14b' not found",
            "HTTP 500 Internal Server Error: llama-server process has terminated: exit status 2",
        ] {
            assert_eq!(read(words), words);
        }
    }

    fn text(chunks: &[Value]) -> String {
        chunks
            .iter()
            .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
            .collect()
    }

    fn reply(parts: &[&str]) -> Vec<Value> {
        let mut lines: Vec<Value> = parts
            .iter()
            .map(|part| json!({"message": {"role": "assistant", "content": part}, "done": false}))
            .collect();
        lines.push(json!({"message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop", "prompt_eval_count": 9, "eval_count": 9}));
        lines
    }

    #[tokio::test]
    async fn a_tool_call_a_small_model_writes_as_text_is_the_call() {
        // Qwen2.5 Coder 7B on Ollama, asked to read a file.
        let written = chunks(reply(&[
            "{\"name\": \"read\", ",
            "\"arguments\": {\"path\": \"calc.py\"}}",
        ]))
        .await;
        assert_eq!(text(&written), "");
        let last = written.last().unwrap();
        let calls = &last["choices"][0]["delta"]["tool_calls"];
        assert_eq!(calls[0]["function"]["name"], "read");
        assert_eq!(calls[0]["function"]["arguments"], "{\"path\":\"calc.py\"}");
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        // In a fence, in tags, two at once, Llama's `parameters`.
        let fenced = chunks(reply(&[
            "```json\n{\"name\": \"list\", \"parameters\": {}}\n```",
        ]))
        .await;
        assert_eq!(
            fenced.last().unwrap()["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "list"
        );
        let tagged = chunks(reply(&["<tool_call>\n{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}\n</tool_call>\n<tool_call>\n{\"name\": \"read\", \"arguments\": {\"path\": \"b\"}}\n</tool_call>"])).await;
        let calls = &tagged.last().unwrap()["choices"][0]["delta"]["tool_calls"];
        assert_eq!(calls.as_array().map(Vec::len), Some(2));
        assert_eq!(calls[1]["index"], 1);
    }

    #[tokio::test]
    async fn json_that_is_no_call_stays_the_reply() {
        for reply_text in [
            // A tool the request didn't offer.
            "{\"name\": \"deploy\", \"arguments\": {}}",
            // JSON the person asked for.
            "{\"name\": \"Ada\", \"age\": 36}",
            // A call with words after it.
            "{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}} and then I will edit it",
            "```python\ndef median(values):\n    pass\n```",
            "<b>bold</b>",
        ] {
            let chunks = chunks(reply(&[reply_text])).await;
            assert_eq!(text(&chunks), reply_text);
            assert_eq!(
                chunks.last().unwrap()["choices"][0]["finish_reason"],
                "stop"
            );
        }
        // Ordinary text streams as it comes.
        let plain = chunks(reply(&["The bug ", "is in add()."])).await;
        assert_eq!(plain[0]["choices"][0]["delta"]["content"], "The bug ");
    }
}
