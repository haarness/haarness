# Model catalogs

How Haarness decides which models exist, what they are called and what they cost:
live discovery and its timing, the fallback catalog, display names, the reviewed price
table, and a dated record of provider request changes. For contributors who change
providers or catalogs, and for anyone working out why a model is or is not listed.

> **User guides:** [Choose a provider and model](https://haarness.ai/docs/providers/overview), [What a model request costs](https://haarness.ai/docs/cloud/model-costs).

Live discovery determines which models are available. A successful list replaces
previous membership, including when the provider returns an empty list. Static
entries do not resurrect models removed from a live response. The embedded catalog
supplies missing display names, context/output limits and prices; values returned
by the provider remain authoritative. Explicit user configuration still wins.

The server discovers at startup and refreshes stale catalogs on `GET /v1/providers`
(five-minute cache), every provider at once. A server starts answering after three
seconds at most, and a read answers after three seconds with the lists it has, while
slower lists keep loading; a read while they still load answers at once. A read waits
longer (each list gives up within ten seconds) only for a signed-in provider with no
model to show yet, such as Haarness Cloud or an endpoint that lists its models only
when asked, so `haarness run` and a new chat never choose a model without it. When the
lists are all in and the catalog changed, `assets.updated` (`resource: "provider"`)
tells clients to read it again, so an open model picker fills in by itself. A
configuration reload keeps each provider defined as before (the same configuration
section and stored API keys, and still signed in or still not, by OAuth included), its
loaded list with it, and loads the others' lists the same way, after it is applied: xAI
signed in by OAuth after the start is built again, so it lists its models.
The TUI's first frame waits a second at most for the catalog; until it comes the
status line says **Loading models…**, and a new chat the TUI opened meanwhile on a
model it chose takes the model it would have chosen once the catalog is in. `POST /v1/providers/refresh` (a picker opening, a key just saved)
waits for every list; concurrent requests are coalesced with a one-second minimum
interval. Each full request, including pagination, is bounded to ten seconds. Errors, partial pages
and invalid responses retain the last successful catalog. Before any successful
fetch, configured/embedded models provide the offline fallback. The cache is held
in memory for the server lifetime.

Live refresh is implemented for OpenAI API keys and the ChatGPT account catalog,
Anthropic, Gemini, GitHub Copilot and compatible model-list endpoints, including
detected local runtimes. Explicit model allowlists and configured cloud
deployments keep their configured models; of the compatible presets only
Perplexity, which has no list route, keeps a fixed list. OpenAI API-key and ChatGPT membership and
reasoning options have separate caches: an API option is not automatically an
option on a subscription. The
server advertises `providers.refresh.v1`; older remote servers remain usable.

Desktop and TUI model pickers contain connected providers only; with nothing
connected, `/models` and the composer's model pill open the connect flow
instead. Desktop resolves
removed/disconnected selections to an available model and sends the pair actually
shown in the composer. With no available model it opens **Connect a provider** while
preserving the chat draft. Desktop opens a project without waiting for its model
lists; a message sent while they still load waits for them, so it goes to the model
the composer then shows, and is given up if another chat or project is shown
meanwhile. Schedule pickers also use connected models and validate the choice
before saving. Refreshing the picker does not interrupt running turns.

Desktop's **Settings → Providers** lists the connected providers, each with its
models behind a count, plus **Refresh models** (`POST /v1/providers/refresh`);
the full catalog is in **Connect a provider**. **Settings → Models** lists the
connected providers' models, and **Show all** adds the rest. From the CLI,
`haarness models [PROVIDER]` prints the catalog the server would use, with live
discovery where credentials allow; `--verbose` prints context, output,
capability and pricing metadata as JSON.

Display names are centralized in `crates/haarness-protocol/src/model-names.json`,
shared by the Rust protocol helper and Desktop. Provider-supplied names take
precedence; unfamiliar IDs are preserved without guessing a label. A Bedrock Claude
ID is read by the bare ID it names, whatever its region prefix and version tag
(`apac.anthropic.claude-sonnet-5-5-v1:0` is Claude Sonnet 5.5). IDs remain the
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

A provider's announced price change goes in the entry's `changes`, so nobody has
to edit the table on the day:

```json
"changes": [
  {"from": "2026-12-01", "input": "4", "output": "20", "cache_read": "0.2", "cache_write": "4"}
]
```

Each change lists every rate billed from 00:00 UTC on its `from` date,
`long_context` included (a change without one ends the tier), in date order, one
per date. The latest change whose date has come replaces the entry's own rates;
before the first, the entry's own rates apply. Cloud prices each pricebook import
by its UTC date, and its import task wakes just after midnight UTC when a change
is due, publishing a new pricebook version within a minute or so of the change;
the app prices by the computer's clock in UTC. Once a change is in force and has
been checked on the provider's page, fold it into the entry's own rates.

Nothing checks these prices automatically. Monthly, and when a model ships, run
`python3 scripts/check-model-prices.py`. It lists table entries whose OpenRouter
price now differs (the provider changed its price, or OpenRouter is wrong; an
entry's `note` explains a known difference), entries not checked for 30 days, and
recent models priced by OpenRouter, with where the app's fallback disagrees. It
also lists every scheduled change and flags one due within 14 days (`--due`), so
its price is confirmed on the provider's page before Cloud starts billing it, and
one that adds or ends a long-context tier. The script reads OpenRouter's catalogue
but never a provider's page: confirming a price is a person's job. Fix or add the
entry in haarness-cloud from the provider's page with today's `checked` date,
then `--sync` copies the table here.

## Provider fixes: 2026-11-08

Found by running coding chats on each provider with the owner's own keys, on the released 0.1.16 and on main.

| Provider | Change |
| --- | --- |
| Gemini | Google still lists Gemini 2.5 to every key but refuses it to accounts made since ("no longer available to new users … use models/gemini-3.1-pro-preview"): the chat says Google no longer offers it to new accounts and names the model Google suggests. A free-tier key asking for a model its tier leaves out (`limit: 0`, Gemini 4 Argon among them) failed after waiting the 41 seconds Google names four times; it now fails at once and says to choose a Flash model or turn on billing in Google AI Studio. A day's free allowance used up says when it comes back. No provider's quota of 0 is waited for. |
| xAI, OpenAI (ChatGPT sign-in) | A sign-in whose renewal the provider refuses (a refresh token that expired, was revoked or was spent elsewhere) says to connect the provider again, not "refresh token returned 400 Bad Request: Invalid or unknown refresh token". |
| OpenAI (API key) | `insufficient_quota` says the API account has no credit or is at its spending limit, where to add credit, and that signing in with ChatGPT is the other way in: a ChatGPT plan doesn't pay for API keys. |
| Mistral | A reasoning model's thinking, streamed a few words a part, goes back in later requests as one part instead of one per chunk, which made it four to six times larger in every later request. |
| Gemini | A new Gemini connection, and a chat that names no Gemini model, starts on Google's newest Flash model (Gemini 3.8 Flash today), which free Google AI Studio keys can use; Gemini 4 Argon is listed next and stays choosable. A model someone chose, or set as the default, stays as it is. |
| All | `marketplace_manage` and `capability_manage` wait behind `load_tool` like the other managers instead of going out with every request (4.5 KB). |

## Provider fixes: 2026-11-07

| Provider | Change |
| --- | --- |
| Anthropic, OpenAI, Bedrock, Chat Completions | Audio, video or any other file a model can't take no longer fails the request when it came from a tool (an MCP server's recording) or with an earlier message: a one-line note stands in for it, and the chat goes on. Before, the request failed again on every later turn, until the chat changed model. Only the person's newest message is refused, naming the model ("Haarness can't send audio to Claude Sonnet 5.5. Send the message without the recording, or switch to a model that takes audio."), and the chat shows those words without "Provider error:". Chat Completions sends MP3 and WAV as `input_audio` only to a model whose listing says it takes audio, or, where the listing doesn't say, to a family known to: Gemini, OpenAI's audio models, Voxtral and the omni models. On Haarness Cloud only `google/` and `openai/` models get it, since Cloud's router sends every other vendor through a route that takes no audio: a Cloud Voxtral or omni model gets the note too. Everything else, Haarness Cloud's Claude, GPT and DeepSeek among them, gets the note, since a model that takes no audio refuses the request. A Bedrock model listed without image input (gpt-oss, a text-only Llama), or an Anthropic-compatible model set up with `images = false`, gets pictures the same way, as Chat Completions models already did. Gemini and Vertex AI take every type Haarness attaches, so nothing changed there (`Unreadable` in `crates/haarness-providers/src/media.rs`). |
| Haarness Cloud | Cloud's own 429s no longer fail a turn at once. A sign-in already running as many requests at once as its key allows (eight by default, which a fan-out of agents can pass) or a full router is tried again after 2, 4, 8 and 16 seconds; a key's used-up minute (300 requests) waits for the `Retry-After` Cloud names. Each refusal reaches the turn a random part of a second late, so agents refused together don't try again together, and a full router's own `Retry-After` is kept. If Cloud is still busy, the reply says so and what helps. Cloud's cap on an account's requests in flight still ends the turn, now in plain words: wait for other requests to settle, or, if none are running, lower the output limit or shorten the conversation. `scripts/test-cloud-busy-retry.py` drives the server against a Cloud stand-in. |
| OpenRouter, Haarness Cloud, xAI, DeepInfra | A catalog price that isn't a finite number once in USD per million tokens (`NaN`, `inf`, a per-token `1e303`), or that is below zero, counts as no price, instead of a cost of NaN or infinity on every turn. |

## Provider fixes: 2026-11-01

Found by running real coding chats on each provider, on every client.

| Provider | Change |
| --- | --- |
| Vertex AI | A Google API key (`AIza…`, or `AQ.…` for a key bound to a service account) is sent in `x-goog-api-key` and needs no project; without one, requests go to the global publisher endpoint. The location defaults to `global`, where current Gemini models are served. A pasted key is checked before it is saved, and a key Vertex does not take (most often a Gemini key from AI Studio) says so plainly. Vertex's 429 "Resource exhausted. Please try again later." is retried with backoff. |
| Gemini and Vertex | A message sent after tool results, as after a stopped tool call, starts its own content: Vertex refused text after a function response in one content. A request with no tools (compacting a chat) sends no `tools` field, which Vertex refused when empty. |
| All Chat Completions providers | A 429's `Retry-After` of whole seconds is passed on, so the turn waits and retries instead of failing on "Rate limit exceeded". A request larger than the plan's tokens a minute (Groq's free tier allows 8,000) says it can't be sent and where to upgrade. |
| Mistral | Large 4 takes `low`/`medium`/`high` (Mistral now refuses `none`); Medium 3.5 and Small 4 take `none`/`high`. Small 4 has a price and Large 4 uses its current list price ($0.70/$2.10). Mistral's `name` field (the id an alias points to) is not used as a label, and alias ids take their model's levels and price. |
| Groq | Speech-only models (top-level `output_modalities`) stay out of the chat picker, and names leave out the vendor prefix. |
| Z.ai | GLM 5.3 and its Flash models always think and take `low`/`high`/`max`; GLM 5.2 takes every level; older GLM models switch thinking with `thinking`. Every listed GLM model has Z.ai's list price, and a new connection starts on GLM 5.3. |

## Provider requests: 2026-10-18 to 2026-10-22

No catalog or price entries changed; these change what Haarness sends.

| Area | Change |
| --- | --- |
| Reasoning between tool calls | A server the user set up (a configured custom provider, the local Ollama and LM Studio runtimes, `OPENAI_COMPATIBLE_*`) gets the model's reasoning back on the assistant message that carries a tool call, in the field it streamed it in (`reasoning_content`, `reasoning` or `reasoning_text`); structured `reasoning_details` stay the replay when the server sent them. DeepSeek, Moonshot and Z.ai still receive `reasoning_content` on every assistant message. Built-in services and managed gateways (Azure OpenAI, GitHub Copilot, routers) get nothing back their API does not document. Stored reasoning names the provider that produced it and goes back only to that provider, so a chat moved from DeepSeek to another endpoint does not send DeepSeek's reasoning there (`ReasoningReplay` in `crates/haarness-providers/src/openai_compatible.rs`). |
| Blank text | Claude refuses a text block that is empty or only whitespace with a 400, which once broke a chat whose model sent `"\n\n"` before a tool call. Whitespace-only replies and blank user messages are no longer sent to any model, and a turn started over the API with blank text, no attachment and no command is refused. The Anthropic adapter leaves out blank text beside an image and in an image-only tool result; Bedrock does the same, and sends `(document attached)` where a document needs text and `(no output)` for an empty tool result. `scripts/test-claude-blank-text.py` drives the built-in Anthropic provider against a mock. |
| Bedrock names | A Bedrock Claude id is named by the bare id it carries, whatever its region prefix and version tag (see the display names above); `scripts/test-model-labels.py` checks it through the real CLI and server with no network. |

## Gemini 4 Argon: 2026-10-15

Gemini 3 and every later generation (the version is read from the id) take
`thinkingLevel`; Gemini 2.5 and older, and unversioned ids such as
`gemini-flash-latest`, keep `thinkingBudget`. Gemini 4 Argon (`gemini-4-argon`)
answers `minimal` with a 400 on the Gemini API, as 3.1 Pro and 3.7/3.8 Flash
do, so Gemini 4 and later offer low, medium and high, and a `none` or `minimal`
effort is sent as `low`. The Gemini API still accepts a `thinkingBudget` for
Argon, but through Vertex any budget is refused, so Haarness never sends one.

A `thinkingBudget` counts against the request's output limit. When a request
sets one (`maxOutputTokens`), the budget is at most half of it, so a short
answer under a small limit (Auto's review verdict asks for 2048 tokens) still
has room after the model thinks. Thinking that was asked for stays on unless
half the limit is less than the model thinks at all: then 2.5 Pro, which cannot
turn thinking off, thinks its minimum of 128 tokens, and a model that can turn
it off does so (2.5 Flash-Lite thinks at least 512 tokens when it thinks).

Argon is in the catalog with a 1,048,576-token window and the 32,768 output
tokens the Models API lists. Probes found tools in `AUTO` and forced `ANY`
modes (including the array bounds that break forced calling on older models),
JSON-schema output, images, PDFs, `countTokens`, `temperature` and implicit
cache hits all working; there is no explicit caching. Its reviewed price is
$2/$10 per million (cached input $0.10) through 2026-11-30 and $4/$20 (cached
input $0.20) from 2026-12-01, with no long-context tier. The entry schedules the
December price in `changes` (see [Prices](#prices)), so Cloud bills it from
00:00 UTC that day without an edit; Gemini 3.8 and 3.6 Flash schedule their
2027-01-01 doubling the same way.

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
`--scenarios coding` has the model fix a failing unit test (the shell, a read or
search, an edit, the shell again) and then add a function with its test in a
second turn, and the script runs the tests itself after each; `long` sends a
message of about 30,000 tokens. Writing a model as `provider/model>provider2/model2`
switches the chat between the coding turns, as the picker does.
`--global-config` gives the private profile a global configuration, where a
provider's `base_url` can point at a recording proxy.

The results and account limitations for this update are recorded in
[the model-support audit](records/2026-09-23-model-support.md).
