# DeepSeek, live — 2026-10-31

DeepSeek had never run live (no key in the [2026-10-10 audit](2026-10-10-model-support.md)). With one, its two
models were run against DeepSeek's own API, directly and behind a self-hosted Haarness Router, on every layer the
[OpenRouter run](2026-10-31-openrouter-live.md) covered. $0.24 of DeepSeek credit was spent, all off-peak.

DeepSeek's `/models` lists `deepseek-flash` (V4.1, text and images) and `deepseek-v4-pro` (text), both with a
1,048,576-token window, 393,216 output tokens and effort levels `low`, `high`, `max` (`effort.supported_levels`).
The legacy names `deepseek-v4-flash` and `deepseek-v4-flash-vision-exp` are still accepted and served by Flash.

## What was wrong

| Finding | Effect | Now |
| --- | --- | --- |
| DeepSeek refuses a forced tool call in thinking mode ("Thinking mode does not support this tool_choice"), and it thinks unless told not to. | Structured output (workflow agents) failed on both models. | Not forced unless thinking is off (`none`); the runtime's request and retry carry it. Behind a router too. |
| Prices halve off-peak (peak 01:00-04:00 and 06:00-10:00 UTC on weekdays), so Haarness listed none. | No turn had a cost. | Each request reports its charge at the rates of the hour it ran, cache hits at their own rate; the picker shows peak rates, the most a request costs. Chinese public holidays, off-peak all day, are not known, so a request on one is reported at peak. |
| The listing's name is "DeepSeek-V4.1-Flash". | Read so in the picker and status line. | "DeepSeek V4.1 Flash", "DeepSeek V4 Pro". |
| Image input (top-level `input_modalities`) and levels (`effort.supported_levels`) were read only from Haarness's built-in table. | Behind a router Flash took no images and offered no levels. | Read from the listing wherever it appears. |
| Behind a router, DeepSeek's reasoning never went back after a tool call. | 400 "The `reasoning_content` in the thinking mode must be passed back"; turns with tools failed. | A `deepseek/` model behind a Haarness Router gets its reasoning back on every assistant message, as from DeepSeek itself. |
| In a structured turn, text the model wrote before a tool call was saved after it (left unforced, DeepSeek writes "I'll read the file first." beside the call). | One run in three refused: "An assistant message with 'tool_calls' must be followed by tool messages". | Narration is saved before its call; for OpenAI-compatible providers text that came with calls joins their message. |
| A provider's 402 read "DeepSeek rejected the request: Insufficient Balance". | Did not say what helps. | "Your DeepSeek account is out of credit (Insufficient Balance). Add credit with DeepSeek, then try again." (OpenRouter too.) |

Haarness Router (its own repository) changed as well: DeepSeek's profile now carries reasoning and image input
(an `openai-compatible`-style profile promised text and tools only), `OPENROUTER_API_KEY` turns on OpenRouter as a
standard provider with what it preserves, OpenRouter's listing leaves out batch-only and media models, and an
OpenRouter target takes back any vendor's reasoning, so Gemini through OpenRouter keeps working after its first
answer.

## Live verification

`scripts/test-models-live.py`, "All" as in the OpenRouter record (tools over two turns, reasoning at the highest
level, images, a workflow agent's structured output, cost, and a level the model lacks).

| Path | Model | Result |
| --- | --- | --- |
| DeepSeek API | `deepseek-flash` | All |
| DeepSeek API | `deepseek-v4-pro` | All but images (text-only) |
| Haarness Router → DeepSeek | `deepseek/deepseek-flash` | All |
| Haarness Router → DeepSeek | `deepseek/deepseek-v4-pro` | All but images |
| Haarness Router → OpenRouter | `anthropic/claude-haiku-5.5`, `openai/gpt-6-luna`, `google/gemini-3.8-flash` | All |

Recovery through a fault-injecting proxy in front of DeepSeek: a 503 is retried and answered; a 402 and a 429
fail with DeepSeek's words; a stop during reasoning keeps the reasoning and the next message is answered; one chat
moved DeepSeek Flash → Claude (Anthropic API) → DeepSeek V4 Pro → GPT-6 Luna (OpenRouter) → DeepSeek Flash with a
read tool call each time, every switch answered.

Surfaces: the terminal UI (picker names and peak prices, a tool turn with visible thinking and a cost), `serve`
with `run` and `stats` (both models priced, no unpriced records), and `haarness provider connect-router` against a
local router listing 368 models.

## Left for a decision

- The router names an upstream refusal only by status and code ("upstream returned HTTP 400
  (invalid_request_error)"); the provider's message is withheld on purpose, since it may echo a prompt or a key.
  For a self-hosted router that hides the reason a request failed.
- (Resolved below: Cloud with the account's own DeepSeek key works end to end since Cloud 20261031-b79f9a8.)

## Haarness Cloud with the account's own key

A BYOK router, "DeepSeek (own key)" (`b94770b0-fe3d-436f-ac00-c9f01897643b`, left in place on orex@haarness.ai),
was made and keyed without a web form: `POST /api/v1/routers` with `credentialMode: "byok"`, then
`PUT /api/v1/routers/{id}/credentials/deepseek` with the key read from the local credential store by a script.
Through it Haarness lists `deepseek/deepseek-flash` and `deepseek/deepseek-v4-pro`.

- `deepseek-v4-pro` asked for an OpenRouter key, although the router held a DeepSeek one: the platform prices
  that id under OpenRouter, which reaches every vendor it has no key for, and a BYOK request took the provider
  from the price. A model priced under an aggregator by its whole id now goes to the router's own key for its
  vendor, with the vendor's own model id (Cloud 20261031-00a1a38).
- Both models are then refused before DeepSeek is asked: "no route can preserve the requested features:
  deepseek/…: missing reasoning". The router's built-in DeepSeek profile (router 4365f30) promises text and
  tools only; Haarness asks for reasoning on every turn of a reasoning model. A router DeepSeek profile with
  reasoning and images (not yet pushed) is what lets these turns through. A turn without reasoning was not run.
- With that profile pushed (router cf03f8c) the refusal stayed: Cloud built every BYOK preset but OpenAI,
  Anthropic and Gemini as a generic openai-compatible provider, so the router's DeepSeek kind was never used.
  Presets the router has a kind for (DeepSeek, Groq, Mistral, xAI, Together, Fireworks) now use it, and a BYOK
  OpenRouter key states what OpenRouter keeps (Cloud 20261031-b79f9a8).
- Then, through the BYOK router on production: `deepseek-flash` passed tools over two turns, reasoning at the
  highest level, an image, structured output and cost; `deepseek-v4-pro` the same without the image (4 of 4).

Selecting a router (`haarness account --router`) applies to every client on the computer signed in to that
account, not to one run: while the BYOK router was selected, other local Cloud runs failed with "Add your
provider key".

