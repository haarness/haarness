# Amazon Bedrock, live — 2026-10-31

The [2026-10-10 audit](2026-10-10-model-support.md) left Bedrock unverified: no AWS account. A new account
(us-east-1) with a Bedrock API key was made for this round. Its model list, inference profiles and key
checks answered from the first minute; its models did not. Claude and GPT answered "not available for
this account", and gpt-oss, DeepSeek and Nova "Too many tokens per day" before a single token, which is
how a new account's quotas start. So everything up to the model call ran against the real service, the
calls themselves ran against a local proxy speaking Bedrock's wire format, and no model answered live.
Spend: $0.

## What was wrong

| Finding | Effect | Now |
| --- | --- | --- |
| The picker listed two fixed ids, one of them a Claude Sonnet 4.5 profile AWS now calls legacy. | None of the region's other models could be picked; names were raw ids; no prices. | The region's chat models come from Bedrock's own list (76 here), each under the id a request needs: its own where it runs on demand, else the global cross-region inference profile, else the one for the region's geography. Bedrock's names, list prices, output limits and image support. Embeddings, image and speech generators, speech-to-speech and video-only models, safety classifiers, provisioned-only and legacy models are left out. |
| Answers came from Converse in one piece. | Nothing showed until the whole answer had arrived; a long one waited on a 15-minute limit and was then failed. | ConverseStream, read frame by frame (AWS event-stream framing, checksummed): text, reasoning and tool calls arrive as written, and a silent stream fails at the idle limit like any other provider. |
| No reasoning settings were sent. | Effort levels were missing; Claude 5.5 ran at Bedrock's default. | Levels come from each model's own request schema: `output_config.effort` (Claude 5.x), `reasoning_config` (GPT, Grok, GLM, Kimi), a thinking budget (Claude 4.x, Nova 2). The signed reasoning before a tool call is sent back with it. |
| No prompt caching. | Every turn paid full price for ~25k tokens of instructions and tools. | Cache points on the tools (Claude), system prompt and newest turn, for models that take them; parallel first requests wait for the cache like on Anthropic's API. |
| Refusals read "Bedrock returned 403 Forbidden: {json}", a shape the runtime does not read. | Throttling and outages were never retried; every refusal showed AWS's JSON. | "HTTP 429 Too Many Requests: [ThrottlingException] …": per-minute throttling, a model still loading and 503s back off; a daily token quota fails at once ("Amazon Bedrock rate limit or quota reached: Too many tokens per day…"); a wrong key reads as a refused key. |
| No key check. | A mistyped key was saved and Bedrock read "connected". | Checked on the model list before it is saved; AWS's own words ("Authentication failed: Please make sure your API Key is valid") under the field. A key whose policy leaves out the list is still accepted. |
| SigV4 signed the model id's `:` as is; AWS signs the path encoded once more. Discovery's GETs did not send the content type they signed. | Every id holding `:` (the old default, `amazon.nova-pro-v1:0`) failed to sign with AWS access keys. | Path and query canonicalized as AWS's SDKs do; signatures match botocore byte for byte for a `:` id and a sorted, encoded query. |
| The connect screen asked for a "bearer token" and linked to the console's front page. | | "Use an API key", linking to the console's API keys page. |

## Live verification

| Path | Result |
| --- | --- |
| `ListFoundationModels`, `ListInferenceProfiles` with the API key | 200; `haarness models amazon-bedrock` lists 76 models. |
| Picker (terminal UI, tmux) | "bedrock sonnet" finds Claude Sonnet 5.5, 5 and 4.6 with names, 1M context and prices. |
| Connect (terminal UI) with a made-up, well-formed key | Refused under the field: "Amazon Bedrock rejected this API key (Authentication failed: Please make sure your API Key is valid)". |
| `run` on Claude Haiku 5.5 | "Amazon Bedrock rejected the request: anthropic.claude-haiku-5-5 is not available for this account…" |
| `run` on Nova Micro | "Amazon Bedrock rate limit or quota reached: Too many tokens per day, please wait before trying again." — at once, no retries. |
| `run` on an unknown model | "Amazon Bedrock rejected the request: The provided model identifier is invalid." |

Through a local proxy that forwards the model list to Bedrock and answers ConverseStream itself, in
Bedrock's framing, with the real binary:

| Case | Result |
| --- | --- |
| A well-formed answer | Streamed and shown. |
| 503 ServiceUnavailableException, then an answer | Retried; answered. |
| 429 ThrottlingException (per minute), then an answer | Backed off; answered. |
| `throttlingException` inside the stream after some text | Fails the turn with Bedrock's words, as an error event does for every provider. |
| Stream cut after some text | Continued from what arrived. |
| A tool call, then an answer | The tool ran; answered. |
| Request shape | Cache points (system, newest turn) once the model list has loaded; `maxTokens` 8192; 74 tools. |

## Not verified

- No model answered live: streaming, tool turns, reasoning levels, images, PDFs, structured output,
  cache points and their pricing are covered by fixtures built from Bedrock's documented and listed
  shapes, not by Bedrock itself. The account needs its quotas raised (Service Quotas or a support case)
  and Anthropic's access to go through.
- SigV4 with AWS access keys: matched against botocore, not sent to AWS (only an API key exists).
- Claude's 1M context on Bedrock may need a beta flag; the picker shows the catalog's 1M.
- Models that take no tools on Converse (Llama 3 8B, Mistral 7B, Mixtral) are listed; Bedrock should
  refuse their requests with tools, which would read in Bedrock's words. Not run.
- Prices are list prices; a regional (`us.`) profile costs about 10% more for newer Claude models.
- Desktop and Haarness Cloud were not run on Bedrock.
