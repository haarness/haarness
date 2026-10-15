# Model support audit — 2026-10-15

Follows the [2026-10-10 audit](2026-10-10-model-support.md). OpenRouter's catalogue
and the providers' own model lists (Anthropic Models API, OpenAI and Gemini
`/models`) were checked for models released since then. Gemini 4 Argon
(2026-10-14) is the one new model on a provider Haarness serves directly; the
Anthropic and OpenAI lists have nothing new since GPT-6.1 Sol and Claude Haiku
5.5. Meta's Muse Wonder (2026-10-12, $4/$20 on OpenRouter) is only reachable
through OpenRouter, whose discovery already supplies it; no key was available
to try it.

## What the probes settled

Direct Gemini API requests of a few tokens each (`gemini-4-argon`):

| Question | Answer |
| --- | --- |
| Model listing | `models/gemini-4-argon`, 1,048,576 input tokens, 32,768 output tokens, `thinking: true`; no `createCachedContent`. |
| Thinking levels | `low`, `medium` and `high` work; `minimal` answers 400 ("Thinking level MINIMAL is not supported"), `max` is not a level. The Gemini API also accepts `thinkingBudget`, which Vertex refuses; Haarness sends levels only. |
| Sampling | `temperature` and `topP` accepted. |
| Tools | `AUTO`, forced `ANY` (also with `allowedFunctionNames`) and `VALIDATED` all call the tool. Forced calling accepts the `maxItems` array of `anyOf` objects that older Gemini models refuse. |
| Structured output | `responseJsonSchema` returns valid JSON, also with tools present. |
| Media | PNG and PDF input read correctly; `countTokens` works. |
| Caching | Implicit only: a repeated 9,665-token prompt read 7,616 tokens from cache. |
| Price | Google's page: $2/$10 per million (cached input $0.10) through 2026-11-30, $4/$20 (cached input $0.20) from 2026-12-01; no long-context tier, no free tier. OpenRouter agrees and also lists a $0.1875 cache-write price, which Google does not charge. |

## Changes

- Haarness: `gemini-4-argon` in the Gemini catalog (1,048,576 / 32,768), display names for its bare, `models/` and `google/` ids, and the reviewed price ($2/$10, cache read $0.10, cache writes at the input price) with the December change in its note. The thinking-level handling for Gemini 4 came with the [October 13–14 port](2026-10-15-reliability-port.md). Live-test candidates and the Desktop Cloud effort case include Argon.
- Haarness Router: Gemini 4 and later offer low, medium and high, so a `minimal` or `none` effort is refused by the router with a clear message instead of a Google 400, and a discovered Gemini 4 model counts as a reasoning model. Argon is in the example configurations.
- Haarness Cloud: the same reviewed price table, and a test that Argon bills at Google's rates rather than OpenRouter's cache-write price.
- iOS needs no change: the composer's effort menu comes from the server's model list, and router model ids such as `google/gemini-4-argon` read as "Gemini 4 Argon".

## Live verification

| Path | Result |
| --- | --- |
| Gemini API, `scripts/test-models-live.py --scenarios tools,reasoning,image,schema,cost,fallback` | Passed: three reads over two turns, image, workflow structured output and a price on every turn at `low` ($0.083 with 48,009 cached input tokens); visible reasoning at `high` (734 characters); and `none`, which Argon lacks, sent as `low`. |
| Haarness Router (this change, run locally with only a Gemini key) | `/v1/models` lists Argon with low/medium/high and 32,768 output tokens; `minimal` is refused by the router; `high` answers with visible reasoning. A Haarness server connected to it ran a `read` tool turn on `router/gemini/gemini-4-argon`, priced at the reviewed rates, with 11,779 tokens read from cache on the second round. |

Not run: Argon through a local Haarness Cloud stack (the only one running belongs to
another session) and the live-Cloud Desktop account spec, which needs one.

## Left for a decision

- The price table has no dated prices. Cloud will bill Argon at $2/$10 after Google's
  price doubles on 2026-12-01 unless the entry is updated by hand that day (Gemini 3.8
  Flash has the same kind of note for 2027-01-01). A `from` date per price would let the
  table carry both.
