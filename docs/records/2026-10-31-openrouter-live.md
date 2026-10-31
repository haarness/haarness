# OpenRouter, live — 2026-10-31

The [2026-10-10 audit](2026-10-10-model-support.md) left OpenRouter unverified: no key. With one, every
layer was run against the real service: model discovery, chats with tools over several turns, reasoning,
images, structured output, cost, errors and recovery, the terminal UI, `serve` and the CLI. $1.95 of
credit was spent; a request's prompt is ~25k tokens of Haarness's own instructions and tools.

## What was wrong

| Finding | Effect | Now |
| --- | --- | --- |
| Every request asked for the model's whole output limit (`max_tokens` 65,536 for Gemini Flash Lite). OpenRouter refuses a request the balance or key limit can't fully pay for: 402 "You requested up to 65536 tokens, but can only afford 16000". | A key with a few dollars failed every turn, even a one-word answer. | Asked again once with the output it can afford, when that is at least 4,096 tokens; below that the refusal stands. |
| Routers that pick a model per request (`openrouter/auto`, `pareto-code`, …) list their price as `-1`. | Priced at −$1,000,000 per million tokens. | No table price. |
| OpenRouter reports what each request cost (`usage.cost`), which Haarness ignored. | Routers had no cost; other models were priced from the list, not what the account was charged. | That charge is the request's cost (`ProviderEvent::Cost`). |
| 40 `:batch` variants list no chat route (`endpoints: []`). | In the picker; choosing one failed: "cannot be used with the chat/completions endpoint". | Left out. |
| 14 image and audio generators (Nano Banana, GPT Image, Lyria, GPT Audio). | In the chat picker, which leaves image models out elsewhere. | Left out; the Auto routers stay. |
| The model list answers any key, a made-up one included, and was the key check. | A mistyped key was saved and the provider read "connected". | Checked on `/key`, which refuses an unknown key (401 "User not found"). |
| An upstream refusal reads only "Provider returned error"; the provider's words are in `metadata.raw`. | "OpenRouter rejected the request: Provider returned error". | "Google AI Studio: Request contains an invalid argument." |
| An upstream failure inside the stream (`code: 502`, a raw `data: {"error":…}` from Alibaba) lost its status. | Not retried; the turn failed. | Keeps "HTTP 502 Bad Gateway: …" and is retried. |
| A full shared upstream pool answers 429 "temporarily rate-limited upstream. Please retry shortly" with no wait. | Failed at once with "Provider rate limit or quota reached". | Backed off as a busy provider (2, 4, 8, 16 s); a rate limit's message keeps the provider's words. |
| Structured output forces a tool call; Gemini through a gateway refuses `maxItems` under forced calling. | Workflow agents on Gemini failed (400 invalid argument). | The bound is dropped for that request, as the native Gemini adapter does. |
| Alibaba refuses a forced tool call while Qwen thinks, which its reasoning models do unasked. | Structured output on Qwen failed. | Not forced; the runtime's request and retry carry it. |
| OpenRouter names are "Vendor: Model". | The status line read "openrouter/Anthropic: Claude Haiku 5.5". | "Claude Haiku 5.5"; the id still names the vendor. |
| Model search matched the whole query as one phrase. | "openrouter haiku" found nothing among 365 models. | Every word, in any order, provider name included (terminal and Desktop). |

## Live verification

`scripts/test-models-live.py` against OpenRouter. "All" means tools (two sequential reads, then a third
in a second turn), reasoning at the highest level, image input, a workflow agent's structured output, a
price on every turn, and a reasoning level the model lacks.

| Model | Result |
| --- | --- |
| `anthropic/claude-haiku-5.5`, `openai/gpt-6-luna`, `google/gemini-3.8-flash`, `x-ai/grok-4.7`, `minimax/minimax-m3.1-flash` | All |
| `anthropic/claude-sonnet-5.5` | All (structured output without a forced call) |
| `deepseek/deepseek-v4-flash`, `z-ai/glm-5.3`, `moonshotai/kimi-k3` | All but images (text-only models) |
| `qwen/qwen3.8-flash` | Tools; structured output request shape fixed and unit-tested, then the upstream pool answered 429 for minutes |
| `mistralai/mistral-small-2603` | All but images: given the image, the model looks for a file instead. It names the colour when asked without tools, so the request is right. |
| `openrouter/auto` | Tools, structured output, cost from the charge |
| `x-ai/grok-build-0.1` | All but reasoning levels (it lists none) |

Recovery, through a local proxy that forwards to OpenRouter and injects one fault:

| Case | Result |
| --- | --- |
| 402 "can only afford 16000" | Asked again with 16,000; answered. |
| 502 inside the stream | Retried; answered. |
| 429 from a shared pool | Named; backed off. |
| 400 "Provider returned error" | "Google AI Studio: Request contains an invalid argument." |
| Stop mid-answer, then a new message | Interrupted, then answered. |
| One chat across Gemini, Claude (OpenRouter and Anthropic's API), GPT-6 Luna, DeepSeek, back to Gemini, a read tool call each time | Every switch answered with the right marker. |
| A made-up key, an unknown model, a model without tools, a 16k-context model | Refused with OpenRouter's words; unknown model named; answers without tools; answers. |

Surfaces: the terminal UI in tmux (connect refuses a made-up key under the field; picker search, no
batch or image models, Auto Router without a price; a tool turn shows tokens and $0.01), and `serve
--daemon`, `run --format json`, `sessions list` and `stats` (Auto Router with a known cost, no unpriced
records).

Haarness Cloud, live: this Mac connected as a whole-computer host (Desktop and the CLI built from main,
signed in to production), driven from haarness.ai in Chrome. The web picker lists an OpenRouter group with
the plain names; a turn on Claude Haiku 5.5 through OpenRouter read `Cargo.toml` with a tool call and was
priced on the host ($0.0056); Stop ended a streaming answer; the same chat then switched to Gemini 3.8 Flash
and read another file.

## Left for a decision

- A 429 that names neither a wait nor "shortly" still fails at once, for every provider.
- Haarness Cloud workspaces reach OpenRouter only through a Cloud router holding the person's key; that
  path was not run.

## Fixed after the live run

- A stopped answer kept none of what streamed (every provider, native Anthropic too) and the web app showed
  the stop as a red error card. The streamed text and reasoning now stay, ahead of the stop marker the
  terminal, Desktop and the phones already read as "stopped"; the web app reads it as Stopped too
  (Haarness Cloud). Proven live in the terminal on OpenRouter: the paragraph that streamed stays, the turn
  reads "interrupted", and the next answer knows what it had written.
- The web app's model search matches every word in any order, across label and group (Haarness Cloud).
- `haarness remote run` and `enable` name a new host after the computer, and `remote hosts` and `remote
  chats` list one readable line each (`--json` for scripts).

## OpenRouter in Haarness Cloud

The managed router held keys for OpenAI, Anthropic and Google only. It now sends every other vendor to
OpenRouter with the platform's key, so a signed-in person uses those models on Cloud credits with no key of
their own. OpenAI, Anthropic and Google stay direct.

- Routing: a `"*"` entry in the router's provider map names an aggregator for every prefix the map leaves
  out, by the model's whole id (`x-ai/grok-4.7`); a direct provider gets its own id as before.
- Catalog: only models that can work as an agent's are imported: batch-only variants, image and audio
  generators, OpenRouter's own routers and models without tool calling stay out (257 of 456 offered).
- Capabilities: an `openai-compatible` provider promised text and tools only, so every turn that asked for
  reasoning was refused ("no route can preserve the requested features"); a provider can now state what it
  preserves, and OpenRouter keeps reasoning, its replay and images.
- Reasoning OpenRouter sends as text and again as details arrived at clients as two chunks and showed twice;
  the router now sends them as one (Haarness Router; Cloud follows).

Live through Cloud on production (`scripts/test-models-live.py --providers haarness`): Grok 4.7, DeepSeek V4
Flash, Kimi K3, GLM 5.3, Mistral Medium 3.5, MiniMax M3.1 Flash, and the direct Claude Haiku 5.5, GPT-6 Luna
and Gemini 3.8 Flash passed tools, two turns, reasoning, structured output, cost and a level the model lacks
(26 of 31). A Cloud workspace on haarness.ai wrote and ran a script on Grok 4.7, then switched to DeepSeek V4
Flash in the same chat and changed it.

Found on the way, fixed in Haarness:

- Meta's Muse takes only an automatic tool choice and a router passes the refusal on as a bare 400: a forced
  call refused with 400 is asked again unforced.
- A structured-output answer that called tools and narrated them ran none of the calls and asked again,
  leaving them unanswered, which strict providers refuse; the calls now run.

- A chat that switched models on one router (Cloud: same provider, another model) replayed the first
  model's reasoning details to the next; the router turned DeepSeek's unsigned details into Claude thinking
  and Anthropic refused every turn after the switch (400). A model's details now go back to that model alone.
  Proven against production: unfixed, DeepSeek then Claude Haiku in one Cloud thread fails with that 400;
  fixed, both turns complete with their tool calls. The managed router now guards this too (Haarness Router:
  each provider gets back only its own reasoning; Cloud 20261031-0df3925-4365f30), so the released 0.1.14 and
  Cloud workspaces complete the same switch on production today.

Re-tested on production Cloud 20261031-0df3925 (router one-chunk reasoning, OpenRouter display names): a Cloud
workspace chat on Claude Haiku 5.5 then DeepSeek V4 Flash shows DeepSeek's thought once and answers with its
tool call; the terminal UI runs a Cloud DeepSeek turn with a tool call and its cost.

Selecting a router for a computer's Cloud sign-in (`haarness account --router`) sends every Cloud model there:
a test router with only a DeepSeek key made every Cloud turn on this Mac fail until the default was selected again.

Left for a decision: the managed router hides upstream error words by design, so an upstream 429 ("retry
shortly") arrives as a generic rate limit, and a refusal as "upstream returned HTTP 400"; every Cloud user's
OpenRouter turn draws on one OpenRouter balance, which needs auto top-up before this is announced.

Then, live on production as Cloud 20261031-0df3925 (router 69c070d):

- Reasoning reaches clients once: Cloud's own stream writer handles the router's reasoning-with-details event
  (one chunk; the details alone when reasoning is excluded) and merges both into non-streaming replies. A
  remote chat on Kimi K3 from a Desktop built before the client fix below shows each thought once.
- Names: 194 of 257 Cloud models reached clients as their raw id (`moonshotai/kimi-k3.1-pro`). The import
  now keeps the name OpenRouter lists, without the vendor the id says ("Kimi K3.1 Pro", "Grok 4.7"), and
  `/v1/models` sends it as `display_name` where the upstream gave none.
- Haarness: the router's "Try again in a moment" is backed off like a busy provider and shown in its words;
  details that repeat the reasoning text a stream just sent are kept for replay, not shown again.

Every default Cloud provider, on production with only a Cloud sign-in: the live suite (`--providers haarness`)
passed 10 of 10 on Claude Haiku 5.5, GPT-6 Luna and Gemini 3.8 Flash (direct) and Grok 4.7 and DeepSeek V4
Flash (OpenRouter): tools over two turns, reasoning at the highest level, images on the vision models,
structured output and cost. `haarness run -m haarness-cloud/<model>` read a file on each direct vendor and on
Grok 4.7; the terminal UI found Grok 4.7 by search with its price and long-context tier and ran a tool turn
($0.04); haarness.ai ran a Cloud workspace on Grok 4.7 (wrote and ran a script) and a remote chat on this
computer on Kimi K3 from the "Haarness Cloud" group, found by "haarness kimi k3".

Not run: Desktop's own window (its server and catalog are the ones above; nothing here drives the Electron
window) and the phones (no runnable simulator build; signing a phone in is the owner's). A Cloud router
that uses the account's own keys (BYOK) can be made and keyed without a web form, through the API the CLI's
sign-in may call (`POST /api/v1/routers` with `credential_mode: "byok"`, then
`PUT /api/v1/routers/{id}/credentials/{provider}` with the key); the CLI has no command for it yet.

Changing model in a Cloud chat (Cloud 20261031-0df3925-4365f30, router 4365f30): a chat replays every
earlier turn's reasoning details, whichever model wrote them. After DeepSeek V4 Flash through OpenRouter,
Claude Haiku 5.5 failed every turn ("upstream returned HTTP 400"): the router made DeepSeek's unsigned
thoughts into Claude thinking blocks. The OpenAI Responses path refused any detail not its own. Each provider
now gets back only the reasoning it can take (Claude's signed or redacted thinking, OpenAI's reasoning items)
and the rest is left out; the failing workspace chat answered on Claude with its DeepSeek history. Bedrock still
refuses any replayed reasoning (no Cloud route goes there).

Phones (Cloud 20261031-b79f9a8, router cf03f8c). The iPhone app (main f67c30b, simulator) in a Cloud workspace:
the picker finds models by any words ("deepseek flash") under Haarness Cloud Router with their names and logos;
DeepSeek V4 Flash through OpenRouter read a file, its thoughts shown once; the same chat then answered on Claude
Haiku 5.5 (direct) and on Grok 4.7 (OpenRouter), each reading the file. The Android app (main 9b77f87,
emulator) opens on sign-in; signing an emulator in to the production account is the owner's, so it was not run
further. Desktop's own window was not driven. The Cloud catalog also offers OpenRouter's free variants
("North Mini Code (free)"), which OpenRouter rate-limits hard.
