# AI assistant

Open CAD Studio can use a language model to turn plain-language requests into
drawing edits (`AICMD`) and to answer questions about the open drawing
(`AICHAT`). Both run on the desktop app. The model never edits the drawing
itself: it proposes automation-API operations, and you review and apply them.

## Setting up a provider — `AISETTINGS`

`AISETTINGS` (alias `AIOPTIONS`) opens the shared settings that every AI
feature uses.

**Providers tab**

| Provider | URL | Key |
| --- | --- | --- |
| OpenRouter | `https://openrouter.ai/api/v1` | Required. Paste it, or set `OPENROUTER_API_KEY`. |
| Ollama (local) | `http://localhost:11434` | None. Run `ollama serve` and pull a model, e.g. `ollama pull qwen2.5vl:7b`. |
| OpenAI-compatible | e.g. `http://localhost:1234/v1` (LM Studio), a vLLM server or a company gateway | Optional, or set `OCS_OPENAI_API_KEY`. |

*Refresh models* lists what the provider offers (OpenRouter also returns
prices and which models accept images; vision models are tagged `[vision]`).
*Test default* sends a one-word request to the default provider and model.

**Features tab** — each AI feature can use its own provider and model, for
example a fast, cheap text model for chat and a vision model for image work.
An empty model uses the provider's default model.

**Privacy & cost tab**

- *Only use local models* refuses every provider that is not on this
  computer (Ollama, or an OpenAI-compatible server on `localhost`), so no
  drawing data leaves the machine.
- *Show a token and cost estimate and ask before each hosted request* makes
  the first **Send** show the estimate and the second one send it.
- *Timeout* and *Max reply tokens* apply to every request.

Settings are saved in `ai_settings.json` in the Open CAD Studio settings
folder (`%APPDATA%\OpenCADStudio` on Windows). API keys are kept in a
separate `ai_keys.json` (owner-only permissions on Linux and macOS), so the
settings file can be shared without leaking a key.

## Natural-language commands — `AICMD`

Describe a change:

> draw a 40×20 rectangle with 3 mm fillets at the origin, then array it 5
> times along X at 50 spacing

The model answers with a plan of operations — `entities_create`,
`entities_transform` (move, copy, rotate, scale, mirror, array),
`entities_delete`, `select`, and `run` for any command line (FILLET, OFFSET,
DIMLINEAR, LAYER, …). The plan is listed step by step. **Apply** runs it as a
single undo step; if any step fails, everything already done is rolled back.
You can keep refining in the same conversation ("make it 6 copies").

Plans may not save, open, close, plot, export, run scripts or load plugins;
those operations and commands are refused before the plan is shown.

The model is given the drawing's units, layers, object counts by type, and the
current selection (with geometry when 20 or fewer objects are selected), so
"array the selected circle 4 times" works without extra steps.

## Ask the drawing — `AICHAT`

Ask a question:

> How many M6 holes are on layer HOLES?
> What is the total length of the polylines on layer CUTTING?
> Which blocks are never inserted?

The model answers by running read-only queries (`entities`, `layers`,
`header`, `query`, `measure`, `audit`, `text_search`, `blocks`,
`get_selection`), shown in the conversation as **Query** lines, at most eight
per question. If you ask for a change, it can attach a plan that you review
and apply exactly like an `AICMD` plan.

`AICMD` and `AICHAT` share one window; switch between them with the buttons at
the top. The footer shows the model, the estimated size and cost of the next
request, and the tokens (and cost, for priced models) used so far.
