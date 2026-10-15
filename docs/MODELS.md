# Model catalogs

> **User guide:** [Choose a provider and model](https://haarness.ai/docs/providers/overview). This page has the engineering detail.

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

The TUI and CLI display routed Cloud models as `haarness-cloud/provider/model`.
Both that prefix and the existing `haarness/` prefix are accepted; persisted
provider IDs and saved favorites remain compatible. The model picker keeps an
active marker and omits the redundant connected status. Dynamically discovered
models are grouped by provider and family, with numeric generations newest first
and larger variants before smaller ones. Existing curated provider order and TUI
favorites/recents retain priority; dated snapshots follow the aliases.

## Prices

A model's price comes first from `crates/haarness-providers/src/model-prices.json`,
the reviewed price table: provider list prices checked by hand against each
provider's price page, with the page and the date on every entry. It is an exact
copy of Haarness Cloud's `config/model-prices.json`, which Cloud bills from, so the
price the app shows is the price Cloud charges; a test fails while the two differ.
A listed model matches by id, alias or dated snapshot (`claude-haiku-4-5-20251001`)
whichever provider serves it. Any other model is priced from `catalog.rs`, a
snapshot of OpenRouter's list prices; Cloud prices those from OpenRouter's live
catalogue. Prices a provider reports with its models (OpenRouter, Haarness Cloud)
win over both.

Nothing checks these prices automatically. Monthly, and when a model ships, run
`python3 scripts/check-model-prices.py`. It lists table entries whose OpenRouter
price now differs (the provider changed its price, or OpenRouter is wrong; an
entry's `note` explains a known difference), entries not checked for 30 days, and
recent models priced by OpenRouter, with where the app's fallback disagrees. Fix or
add the entry in haarness-cloud from the provider's page with today's `checked`
date, then `--sync` copies the table here.

## Gemini 4 Argon: 2026-10-15

Gemini 3 and every later generation (the version is read from the id) take
`thinkingLevel`; Gemini 2.5 and older, and unversioned ids such as
`gemini-flash-latest`, keep `thinkingBudget`. Gemini 4 Argon (`gemini-4-argon`)
answers `minimal` with a 400 on the Gemini API, as 3.1 Pro and 3.7/3.8 Flash
do, so Gemini 4 and later offer low, medium and high, and a `none` or `minimal`
effort is sent as `low`. The Gemini API still accepts a `thinkingBudget` for
Argon, but through Vertex any budget is refused, so Haarness never sends one.

Argon is in the catalog with a 1,048,576-token window and the 32,768 output
tokens the Models API lists. Probes found tools in `AUTO` and forced `ANY`
modes (including the array bounds that break forced calling on older models),
JSON-schema output, images, PDFs, `countTokens`, `temperature` and implicit
cache hits all working; there is no explicit caching. Its reviewed price is
$2/$10 per million (cached input $0.10) through 2026-11-30 and $4/$20 (cached
input $0.20) from 2026-12-01, with no long-context tier: the price table has no
dated prices, so the entry's note carries the change and the table must be
updated by hand on that date.

## API and metadata update: 2026-10-10

New since the last update: Claude Sonnet 5.5 and Haiku 5.5, GPT-6.1 Sol, Gemini
Nano Banana 2.1, Mistral Large 4 and Kimi K3. Each fact below was checked
against the provider's docs and, where a request could settle it, a direct API
probe. The record and the live results are in
[the model-support audit](records/2026-10-10-model-support.md).

| Provider | Change |
| --- | --- |
| Anthropic | Sonnet 5.5 ($2/$10, cache read $0.10) and Haiku 5.5 ($0.10/$0.50) are priced and in the fallback list. Haiku 5.5 is an adaptive-only family: before the live catalog loads it no longer gets budget thinking or `temperature`, both of which it answers with a 400. Prompt-bound thinking (`block_binding: drop_block`) now also covers Sonnet and Haiku 5.5. Haiku and Sonnet 4.5, which take a thinking budget but no `effort`, offer low/medium/high again instead of no reasoning at all. |
| OpenAI | GPT-6.1 Sol is first in the fallback list; its public API accepts low to max, not `none` or `minimal`, and rejects `temperature`. GPT-5.5 left the ChatGPT fallback (it retires there on 2026-10-14; the API keeps it). Every request carries `prompt_cache_key`, and ChatGPT requests also `session_id`: without them the ChatGPT backend cached nothing between rounds. GPT-5.6 Sol is priced at OpenAI's current $4/$20 (OpenRouter lists $2/$10). "Pro" is `reasoning.mode`, not a model id. |
| Gemini | Image models named after the brand (`gemini-nano-banana-2.1`) and the robotics and computer-use previews stay out of the chat picker, as on the Cloud router. Forced function calling (workflow structured output) no longer sends `maxItems`, which Gemini's decoder rejects with "invalid argument" over complex items; the runtime still validates the full schema. |
| Compatible | Mistral Large 4 (`mistral-large-4-0`): name, 1M context, images, `none`/`high` effort, list price. Moonshot's fallback list moved from the retired `kimi-k2.5`/`kimi-k2-thinking` to `kimi-k3`/`kimi-k2.6`. Claude through a Haarness router returns structured output as native JSON (`response_format`) because Opus 5.5, Sonnet 5.5 and Fable 5.1 refuse a forced tool choice. |
| All | A chat keeps its effort when it switches model; a level the new model lacks now becomes the nearest one it lists instead of a provider 400. Prices can carry a long-context tier (`pricing.longContext`): Haiku 5.5 over 100K, GPT-6/5.6/5.5/5.4 over 272K, Gemini 3.1/2.5 Pro over 200K and Grok 4.7 from 200K cost more for the whole request, and turn costs follow it. OpenRouter and Haarness Cloud tiers (`pricing.overrides`) are read too. Routed ids such as `openai/o3` fall back to the bare model's name. |

Prices now come from a reviewed table shared with Haarness Cloud (see
[Prices](#prices)); `scripts/check-model-prices.py` compares it, cache prices and
long-context tiers included, with OpenRouter.
`scripts/test-models-live.py --scenarios tools,reasoning,image,schema,cost,fallback`
runs the scenarios used for this update against any connected provider,
Haarness Cloud included.

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
[the model-support audit](records/2026-09-23-model-support.md).
