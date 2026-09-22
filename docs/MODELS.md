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

Live refresh is implemented for OpenAI API keys, Anthropic, Gemini, GitHub Copilot
and compatible model-list endpoints, including detected local runtimes. Providers
without model discovery (such as configured cloud deployments and the ChatGPT
subscription transport) continue to use their configured/curated fallback. The
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
keys. It starts a private server and sends only synthetic prompts and a temporary
read fixture. It exercises current models, streaming, reasoning, and tool round
trips, and stores credential-free evidence in its temporary output directory.
