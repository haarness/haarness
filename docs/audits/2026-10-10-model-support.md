# Model support audit — 2026-10-10

Follows the [2026-09-23 audit](2026-09-23-model-support.md). Models released
since then: Claude Sonnet 5.5 (2026-09-28) and Haiku 5.5 (2026-10-07), GPT-6.1
Sol (2026-09-29), Gemini Nano Banana 2.1 (2026-10-06), Mistral Large 4
(2026-10-06) and Kimi K3. Facts come from the providers' docs, their live model
lists (Anthropic Models API, the ChatGPT account catalog, Gemini and OpenAI
`/models`) and, where one request could settle a question, a direct probe of a
few tokens. The changes are summarized in
[Model catalogs](../MODELS.md#api-and-metadata-update-2026-10-10).

## What the probes settled

| Question | Answer |
| --- | --- |
| GPT-6.1 Sol reasoning levels (API) | low, medium, high, xhigh, max. `none` and `minimal` answer 400; so does `temperature`. GPT-6 Sol and Luna still accept `none`. |
| Claude Haiku 5.5 | Budget thinking (`enabled`) and `temperature` answer 400; adaptive thinking, `disabled` and forced `tool_choice` work. |
| Claude Sonnet 5.5 | Budget thinking, `disabled`, `temperature` and forced `tool_choice` answer 400 (thinking off is `between_tools`). Opus 5.5 and Fable 5.1 also refuse forced `tool_choice`; Sonnet 5 accepts it. |
| Claude ≥ 4.7 sampling | Opus 4.7, Sonnet 5 and the 5.5 models reject `temperature`/`top_p` as deprecated; Sonnet 4.6 accepts them. |
| Thinking visibility | Without `display: "summarized"`, adaptive thinking comes back as empty blocks (Haiku 5.5, Opus 5). |
| Tokenizer | Haiku 5.5 and Sonnet 5.5 count exactly like Opus 5.5 (`count_tokens`), so the `Claude` estimator family is right. |
| ChatGPT prompt cache | Without `prompt_cache_key` the backend never reused a prefix; with the key alone it hit now and then; with the `session_id` header every round after the first hit (~98%). The public API caches without a key. |
| Gemini forced calling | `maxItems: 100` on an array of `anyOf` objects (`browser_fill_form`) makes `mode: ANY` requests fail with "invalid argument"; `AUTO` accepts it. |
| Prices that disagree with OpenRouter | GPT-5.6 Sol is $4/$20 on OpenAI's page (promotional, through at least 2026-11-21), $2/$10 on OpenRouter. OpenRouter's `-pro` GPT-6/5.6 ids are its names for `reasoning.mode: "pro"`, billed at the base rates. |

## Defects found and fixed

- Haiku 5.5 before the live catalog loaded: budget thinking and `temperature` (both 400).
- Claude Haiku/Sonnet 4.5 offered no reasoning levels after discovery, because Anthropic lists them without `effort`.
- Workflow structured output on Claude Opus 5.5, Sonnet 5.5 (Cloud's default) and Fable 5.1 through Haarness Cloud: forced `tool_choice` (400). Now native JSON through `response_format`.
- Workflow structured output on every Gemini model with the browser tools loaded: 400 under forced calling.
- ChatGPT sign-in: no prompt cache between rounds.
- A chat keeping an effort the new model lacks (`none` after GPT-6 Sol → GPT-6.1 Sol, `minimal` from a Cloud agent default): every turn 400. The runtime now uses the nearest listed level.
- Gemini Nano Banana 2.1 (image model, no function calling) offered as a chat model, natively and on Cloud.
- Moonshot fallback models retired by the provider.
- Price table: Sonnet 5.5 cache read, Haiku 5.5, GPT-5.6 Sol, no long-context tiers; Cloud billed GPT-5.6 Sol at half its cost and tier cache writes at the base rate.
- Cloud router: Claude thinking invisible (no `display`), sampling parameters forwarded to Claude ≥ 4.7, no prefix-binding control, Nano Banana listed.
- Routed OpenAI ids (`openai/o3`, `-pro` models) showed raw ids in Desktop.
- Test harness bugs found on the way: the Desktop Cloud spec expected "Xhigh" (Desktop says "Extra high") and a withdrawn Gemini 2.5 model; the SDK Cloud fixture predated the fixture issuer's consent page; SDK e2e lacked coverage for five operations.

## Live verification

`scripts/test-models-live.py` with the debug build of this change. "All" means
tools (two sequential reads, then a third in a second turn), reasoning at the
highest level with visible reasoning, image input, a workflow agent returning a
file's marker as structured output, and a price on every turn.

| Path | Model | Result |
| --- | --- | --- |
| Anthropic API | `claude-haiku-5-5` | All, plus a level it lacks (`none`) |
| Anthropic API | `claude-sonnet-5-5` | All |
| Anthropic API | `claude-sonnet-5`, `claude-haiku-4-5`, `claude-sonnet-4-5-20250929` | Tools and structured output; reasoning on both 4.5 models |
| ChatGPT sign-in | `gpt-6.1-sol` | Tools, image, structured output, cost, `none` fallback; cache hits on every later round. At max effort the `auto` summary was empty for a short answer — OpenAI's behaviour, also seen on the API with GPT-6 Sol. |
| OpenAI API key | `gpt-6.1-sol`, `gpt-6-luna` (incl. `none`) | All |
| ChatGPT sign-in | `gpt-5.6-luna` | Tools, structured output, cost |
| Gemini API | `gemini-3.8-flash`, `gemini-3.7-flash` (now served by 3.8), `gemini-3.6-flash`, `gemini-3.1-flash-lite` | Tools, image, structured output, cost; reasoning on 3.8 and 3.7 |
| Haarness Cloud (local stack, real providers, API key) | `anthropic/claude-sonnet-5.5`, `anthropic/claude-haiku-5.5`, `openai/gpt-6.1-sol`, `google/gemini-3.8-flash` | All, with visible Claude reasoning and the published long-context tiers in each model's price |

Surfaces:

- **TUI (tmux):** the model picker names and orders Opus/Sonnet/Haiku 5.5 with prices, Haiku 5.5 offers low → max, a tool turn on Haiku 5.5 shows usage, cost and a 1M context meter, and the same chat switches to GPT-6.1 Sol mid-conversation.
- **Desktop (Electron, local Cloud):** fixture sign-in, the Cloud catalog, the effort slider for GPT-6.1 Sol, Sonnet 5.5, Haiku 5.5 and five other models, and live turns on GPT-6.1 Sol, Haiku 5.5, Haiku 4.5 and Gemini 3.5 Flash Lite through the router; no image models in the picker.
- **Cloud:** the live pricebook import lists Haiku 5.5, Sonnet 5.5 and GPT-6.1 Sol, prices GPT-5.6 Sol at $4/$20, and offers no image models.
- **SDKs:** TypeScript and Python generated from the new schema; unit, type, lifecycle and e2e suites pass, the e2e now covering all 124 operations in TypeScript, sync and async Python.
- **iOS:** takes names, levels and prices from the server; HaarnessKit decodes the new catalog unchanged.

## Not verified live

- Mistral Large 4, Kimi K3, xAI and DeepSeek: no keys. Metadata follows the providers' pages; Mistral's pages spell the id both `mistral-large-4-0` and `mistral-large-4`, so both are recognized.
- Opus 5.5 and Fable 5.1 were not re-run natively, to limit spend; their request shapes are covered by fixtures and they share the adaptive path exercised by Sonnet and Haiku 5.5.
- Bedrock and third-party gateways (OpenRouter) no longer force `tool_choice` for Opus 5.5, Sonnet 5.5 and Fable 5.1, and the runtime now asks again when structured output comes back in the wrong shape ([Upstream port](../RELIABILITY-PORT-2026-10-10.md)); neither path was run live (no AWS or OpenRouter key).
- The managed Cloud router serves Anthropic, OpenAI and Google only; Mistral, xAI and DeepSeek models stay disabled in its catalog until it has keys for them.
