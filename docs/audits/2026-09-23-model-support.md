# Model support audit — 2026-09-23

Updated Haarness from `f9b0873ceaef18a4994e2e74a625e532c11a563e` after checking
current provider documentation and pulling OpenCode `dev` to
`2406400f0aeb07b36d0495af4e05aaca49159832`. The implementation and source links
are described in [Model catalogs](../MODELS.md#api-and-metadata-update-2026-09-23).
Per-model results are in [the verification record](2026-09-23-model-support.json).

## Discovery and API behavior

Provider/account model lists determine membership after a successful refresh.
Static names, prices, limits and known-family API rules remain necessary because
many model-list endpoints omit those fields. Explicit configuration takes
precedence. Account lists, including ChatGPT's reasoning levels, are not replaced
with public API assumptions. Successful empty responses remain empty; failed or
partial responses retain the last successful list. These behaviors were already
implemented and were verified by provider/server tests and both model pickers.

The update adds current fallback metadata and fixes reasoning and tool replay for
OpenAI, Claude, Gemini, DeepSeek and Mistral. It preserves older request contracts
and keeps new provider-specific controls scoped to the corresponding provider.
Unknown conversational models can still be discovered without a release.

OpenCode's provider transforms, central model catalog loader and Anthropic SDK
patch were inspected. Relevant behavior was ported to Haarness's Rust adapters:
encrypted Responses reasoning, signed Claude thinking with prefix binding
controls, Gemini thinking levels/signatures, DeepSeek reasoning text and Mistral
streamed content arrays. This is a comparison of the relevant adapters, not a
claim that every OpenCode provider or feature has been ported. The upstream
central catalog URL returned HTTP 403 here; provider-owned catalogs were tested
directly. Current Gemini documentation overrides the generic upstream Flash
effort list where 3.7/3.8 no longer accept `minimal`.

## Live verification

**27 model IDs passed**, using the debug CLI/server built from this change.
Each test starts a conversation on the selected provider/model, reads two
synthetic files sequentially, returns an unpredictable marker, then reads another
file in a second user turn. Persisted tool calls/results, final markers and model
selection are checked. The final successful runs contain **82 tool calls and 54
user turns**. These checks exercise discovery, streaming, reasoning where enabled,
tool argument parsing, tool-result continuation and saved conversation history.

| Provider/authentication | Passing model IDs |
| --- | --- |
| OpenAI via ChatGPT | `gpt-6-astra`, `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna` |
| Anthropic API | `claude-opus-5-5`, `claude-fable-5-1`, `claude-opus-5`, `claude-sonnet-5`, `claude-fable-5`, `claude-opus-4-8`, `claude-opus-4-7`, `claude-opus-4-6`, `claude-sonnet-4-6`, `claude-haiku-4-5-20251001`, `claude-sonnet-4-5-20250929`, `claude-opus-4-5-20251101` |
| Gemini API | `gemini-3.8-flash`, `gemini-3.7-flash`, `gemini-3.6-flash`, `gemini-3.5-flash`, `gemini-3.5-flash-lite`, `gemini-3.1-flash-lite`, `gemini-3-flash-preview`, `gemini-flash-latest`, `gemini-flash-lite-latest` |

The first live pass exposed a Claude Opus 5 tool call that emitted an initial
empty object and no argument delta. The parser incorrectly tried to parse an
empty string. It now retains the initial input when no delta arrives; an HTTP/SSE
regression fixture covers both empty and complete initial inputs, and Opus 5
passed the live retest. Gemini 3.8 Flash and 3.1 Flash Lite returned temporary HTTP
503 overload errors during the full rerun; both passed a sequential retry on the
unchanged binary. Failed attempts remain failures in their original evidence.

## Limits of the verification

- Gemini `gemini-3.1-pro-preview` and `gemini-pro-latest` were blocked by account
  quota/rate limits. Gemini 2.5 Pro, Flash and Flash Lite were listed but inference
  returned HTTP 404 saying they were no longer available to new users. These five
  IDs are **not live passes**. Existing 2.5 request support remains for eligible
  accounts and is covered by request fixtures.
- The ChatGPT account exposes six models. GPT-5.5, GPT-5.4 Mini and GPT-4.1 were
  unavailable there. The available public OpenAI API key returned HTTP 401, so
  public API-only options, including GPT-6 Sol/Luna `none`, were verified with
  local request/stream fixtures rather than live. Subscription effort choices
  remain authoritative.
- No credentials were available for xAI, DeepSeek, Mistral, Groq or the other
  compatible/cloud providers. Relevant API changes were checked against official
  documentation and HTTP/SSE fixtures; those providers are **not live-certified**.
  Z.AI and Moonshot documentation fetches were unavailable, so no new model
  capability claims were added for them.
- Each successful model was exercised with text, file-read tools and its selected
  reasoning level. This does not exhaust every effort, modality, hosted tool,
  compaction scenario or account tier. Claude's documented `drop_block` behavior
  keeps changed-prefix requests valid but can discard preserved reasoning after
  prompt/tool changes. No price is invented when the current rate is unverified
  or cannot be represented accurately by the existing price schema.

## Regression checks

| Check | Result |
| --- | --- |
| Protocol | 20 tests passed |
| Providers, including new HTTP/SSE contract fixtures | 89 tests passed |
| Core | 112 tests passed |
| Server unit and integration suites | 237 tests passed |
| Desktop unit tests | 147 tests passed |
| Desktop TypeScript | Passed |
| Desktop `connected models refresh live` Electron workflow | Passed |
| TUI model picker in real tmux | Passed |
| Debug CLI build and whitespace check | Passed |

The server shell-deadline test timed out in an initial concurrent run; it passed
alone and the complete server suite then passed. The first Desktop E2E attempt
failed while deleting temporary state (`ENOTEMPTY`); its rerun passed. No
remaining failure was found in these regression checks. No dependency or protocol
wire-schema changes were needed.

To reproduce, build with `cargo build -p haarness-cli`, supply provider keys via
the normal environment or an existing Haarness auth store, then run:

```sh
python3 scripts/test-models-live.py --auth-file /path/to/existing/auth.json \
  --providers openai,anthropic,gemini --workers 2
```

Use `--models provider/model,provider/model` for an exact matrix. A missing
explicit model or failed turn exits nonzero. The default matrix also probes older
models and can fail on the documented account restrictions. The harness prints
its temporary evidence directory, containing catalog snapshots, synthetic
transcripts and results. It isolates server/workspace state and cleans up its
private process; credentials are not copied into the verification record. An
existing OAuth store can refresh normally.
