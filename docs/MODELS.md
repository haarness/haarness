# Model catalogs

Live discovery determines which models are available. A successful list replaces
previous membership, including when the provider returns an empty list. Static
entries do not resurrect models removed from a live response. The embedded catalog
supplies missing display names, context/output limits and prices; values returned
by the provider remain authoritative. Explicit user configuration still wins.

The server discovers at startup and refreshes stale catalogs on `GET /v1/providers`
(five-minute cache). `POST /v1/providers/refresh` requests an immediate refresh;
concurrent requests are coalesced with a one-second minimum interval. Each full
request, including pagination, is bounded to ten seconds. Errors, partial pages
and invalid responses retain the last successful catalog. Before any successful
fetch, configured/embedded models provide the offline fallback. The cache is held
in memory for the server lifetime.

Live refresh is implemented for OpenAI API keys and the ChatGPT account catalog,
Anthropic, Gemini, GitHub Copilot and compatible model-list endpoints, including
detected local runtimes. Explicit model allowlists, configured cloud deployments,
and presets with heterogeneous catalogs can deliberately disable discovery and
keep their configured/curated fallback. OpenAI API-key and ChatGPT membership and
reasoning options have separate caches: an API option is not automatically an
option on a subscription. The
server advertises `providers.refresh.v1`; older remote servers remain usable.

Desktop and TUI model pickers contain connected providers only; with nothing
connected, `/models` and the composer's model pill open the connect flow
instead. Desktop resolves
removed/disconnected selections to an available model and sends the pair actually
shown in the composer. With no available model it opens Providers while preserving
the chat draft. Schedule pickers also use connected models and validate the choice
before saving. Refreshing the picker does not interrupt running turns.

Display names are centralized in `crates/haarness-protocol/src/model-names.json`,
shared by the Rust protocol helper and Desktop. Provider-supplied names take
precedence; unfamiliar IDs are preserved without guessing a label. IDs remain the
stable API/storage values and remain searchable in the pickers.

## API and metadata update: 2026-09-23

| Provider | Change |
| --- | --- |
| OpenAI | GPT-6 Sol/Luna names, limits, prices and public-API `none` reasoning; Responses stays the tool transport. GPT-6 reasoning requests omit unsupported sampling parameters. Stateless tool continuations request and replay encrypted reasoning. Voice-only GPT-Live is excluded from the text picker. |
| Anthropic | Opus 5.5 discovered through the live Models API and included in fallback metadata without guessing its price. Adaptive models request visible summaries. Signed/redacted thinking is retained with tool calls; Fable 5.1 and Opus 5.5 use the documented prefix-mismatch control. New models omit incompatible sampling settings; older non-thinking requests keep theirs. Tool calls with no argument deltas preserve the initial input, including `{}`. |
| Gemini | Gemini 3.8 Flash metadata; Gemini 3 uses `thinkingLevel`, while 2.5 retains `thinkingBudget`. Gemini 3.7/3.8 do not advertise `minimal`. Function IDs and signatures survive parallel tool/result replay. Live `thinking=false` clears effort options. Video-generation and transcription-only models are excluded. |
| Compatible providers | Grok 4.7 and Mistral Medium 3.5 metadata and valid effort controls; DeepSeek's current `deepseek-flash` alias and thinking toggle. DeepSeek/Moonshot/Z.AI reasoning text survives tool continuation, and Mistral structured thinking/text chunks are parsed. Other compatible endpoints receive no newly invented reasoning parameters. |

Unknown conversational models still come from successful discovery; these static
additions do not create an allowlist. A model returned by a provider can still be
unavailable for inference due to account quota, deployment restrictions, or a
retired endpoint. Discovery success alone is not end-to-end proof.

Pricing entries describe the documented standard short-context rate, not a full
billing calculator. DeepSeek now has peak/off-peak prices; its old fixed estimates
were removed rather than showing a misleading price.

Compared with OpenCode `dev` at
[`2406400f0aeb07b36d0495af4e05aaca49159832`](https://github.com/anomalyco/opencode/commit/2406400f0aeb07b36d0495af4e05aaca49159832):
Haarness retains provider-owned discovery and adopts the relevant wire-contract
handling. OpenCode fetches a central model catalog; Haarness fetches the provider's
account model list. Current provider docs take precedence where OpenCode's generic
Gemini Flash effort list still includes `minimal`.

Sources checked:

- [OpenAI GPT-6 migration and API requirements](https://developers.openai.com/api/docs/guides/latest-model)
- [GPT-6 Sol](https://developers.openai.com/api/docs/models/gpt-6-sol) and [Luna](https://developers.openai.com/api/docs/models/gpt-6-luna)
- [Claude thinking](https://platform.claude.com/docs/en/build-with-claude/thinking) and [preserved thinking](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking)
- [Gemini thinking controls](https://ai.google.dev/gemini-api/docs/generate-content/thinking) and [Gemini 3.8 Flash](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash)
- [Grok 4.7](https://docs.x.ai/developers/models/grok-4.7) and [reasoning](https://docs.x.ai/developers/model-capabilities/text/reasoning)
- [DeepSeek thinking](https://api-docs.deepseek.com/guides/thinking_mode/) and [current models/pricing](https://api-docs.deepseek.com/quick_start/pricing/)
- [Mistral reasoning and stream chunks](https://docs.mistral.ai/studio/conversations/reasoning)
- [Groq supported models](https://console.groq.com/docs/models)

## Metadata update: 2026-09-08

Added GPT-6 Astra and Claude Fable 5.1; retained the GPT-5.6 Sol/Terra/Luna and
Claude Opus 5/Sonnet 5 families. Claude Mythos 5.1 has metadata for accounts that
return it dynamically, without advertising invitation-only availability in the
fallback list. Current Claude discovery also supplies context/output limits and
supported effort levels. Adaptive thinking follows live capabilities when present,
with known-family fallback; adaptive models use native structured JSON output.

Sources:

- https://developers.openai.com/api/docs/models
- https://developers.openai.com/api/docs/models/gpt-6-astra
- https://platform.claude.com/docs/en/models/overview
- https://platform.claude.com/docs/en/models/fable-5-1/overview
- https://platform.claude.com/docs/en/build-with-claude/structured-outputs

## Verification

Provider/cache unit tests cover complete/partial/empty lists, last-success
retention, concurrent refresh, live metadata precedence, stored API keys and
adaptive Claude requests. Desktop's `connected models refresh live` end-to-end
case drives provider setup, discovery, an old conversation's fallback, an actual
streamed provider response, catalog removal, failure and empty-list behavior.

`scripts/test-models-live.py` is an opt-in live-provider test using environment
keys and/or `--auth-file` pointing to an existing Haarness credential store. A
supplied store can refresh OAuth credentials normally. It starts a private server
and sends only synthetic prompts and temporary read fixtures. Each model must
read two files sequentially, return an unpredictable marker, then execute a new
read in a follow-up turn. Persisted tool calls/results and the selected model are
checked. It stores credential-free evidence in its temporary output directory.
`--providers`, `--models`, and `--workers` bound the test matrix. Missing explicit
models and failed turns produce a failing exit status; they are never reported as
passes. API-key-only options can differ from a ChatGPT subscription's live list.

The results and account limitations for this update are recorded in
[the model-support audit](audits/2026-09-23-model-support.md).
