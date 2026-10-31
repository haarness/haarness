# Ollama and LM Studio, live — 2026-10-31

Haarness finds Ollama (port 11434) and LM Studio (port 1234) on the computer by itself and offers their
models with no key. Both were run on this Mac (M5 Max, 128 GB) with popular open models, one model
loaded at a time, as in the [OpenRouter](2026-10-31-openrouter-live.md) and [xAI](2026-10-31-xai-live.md)
runs. Haarness Cloud does not reach a person's local runtime; a chat on this computer driven from
haarness.ai does, since it runs here.

Models: on Ollama, Qwen 3.8 27B (thinking, images, tools), Qwen3 Coder 30B (tools), gpt-oss 20B (thinking,
tools) and DeepSeek R1 14B (thinking); on LM Studio 0.4.28, Qwen3 Coder 30B A3B.

## What was wrong

| Finding | Effect | Now |
| --- | --- | --- |
| Ollama's OpenAI-style list names models and nothing more. | Every Ollama model was text-only, without thinking or a context window; a photo for Qwen 3.8 was refused. | Each model's `/api/show` gives images, tools, thinking and its context window; thinking models offer `none` to `high` (gpt-oss `low` to `high`); the price is nothing. |
| A runtime started after Haarness was found only on a restart. | Opening Ollama or LM Studio later left their models out until Haarness restarted. | The server notices either runtime starting or stopping within a few seconds and reloads its providers. |
| A picture anywhere in a chat failed every turn on a text-only model. | One photo sent to Claude, or a screenshot a tool returned, made the rest of the chat fail after switching to a local text model. | Only a picture in the newest message is refused ("Qwen3 Coder doesn't read images…"); earlier and tool pictures become a short note to the model. |
| LM Studio loads a model on first use with its default context (8K here). | Every request was refused: Haarness's instructions and tools alone are over 20K tokens. | A model that isn't loaded, or is loaded with under 32K, is loaded with 64K (or its longest) before the request. It stays loaded, as one loaded in LM Studio does. |
| LM Studio answers a model it doesn't have with whichever model is loaded. | A typed or deleted model name was answered by a different model. | Refused, with `lms get` to download it. |
| LM Studio's OpenAI-style list has only ids. | Raw ids, no context window, no price (cost unknown). | Names, context, image support and a zero price from LM Studio's own list (`/api/v1/models`). |
| A model Ollama hasn't downloaded. | "model 'llama3.2:1b' not found". | The same, plus `ollama pull llama3.2:1b`. |
| A chat whose runtime has stopped. | "invalid request: unknown provider ollama" in every client. | "Ollama isn't running. Start it on the computer this chat runs on, then send your message again."; `haarness run` adds how to start it. |
| The terminal UI's home screen named an LM Studio model by its id. | The id beside the name the status bar showed. | The name, as the picker shows it. |

## Live verification

`scripts/test-models-live.py` with only that runtime switched on, one model at a time:

- Qwen 3.8 27B on Ollama: tools over two turns, thinking, a photo, a workflow agent's structured output,
  cost ($0) and a model switch, at `low`, `high` and `minimal`.
- gpt-oss 20B on Ollama: all of those but the photo (it reads text only).
- Qwen3 Coder 30B on Ollama and on LM Studio: tools, structured output, cost and a model switch. On LM
  Studio the run began with the model loaded at 8K; Haarness loaded it again at 64K.
- DeepSeek R1 14B calls no tools, even in a direct request to Ollama with one tool: it is left to chats
  without tools.

Failure cases: a model Ollama lacks, a model LM Studio lacks, a photo for a text-only model then a plain
follow-up in the same chat, Ollama stopped under a running chat and started again (the chat went on
without restarting Haarness), and LM Studio's server off.

Surfaces: the terminal UI on Qwen 3.8 (Ollama) and Qwen3 Coder (LM Studio), each picking the model in the
picker and answering with a read tool call; `haarness run` for each failure case.

While idle, Haarness asks a runtime nothing: its models are read at start, on a reload and when a client
lists models.

## Left for a decision

- A model Haarness loads in LM Studio stays loaded until LM Studio unloads it; its load API takes no idle
  timeout.
- Ollama lists models by their tags (`qwen3-coder:30b`), which is how people pull and run them.
