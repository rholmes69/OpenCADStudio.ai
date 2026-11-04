# AI assistant

Open CAD Studio can use a language model to turn plain-language requests into
drawing edits (`AICMD`), answer questions about the open drawing (`AICHAT`),
turn images into geometry (`AIIMAGE`, `AISKETCH`), check drawings against a
standard (`AICHECK`), suggest sketch constraints (`AICONSTRAIN`), design
parametric parts from a description (`AIPART`), lay out floor plans
(`AIPLAN`) and make dimensioned drawing views of a solid (`AIVIEWS`). All of
them run in the desktop app. The model never edits the drawing
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

## Image to model — `AIIMAGE`

Choose an image that shows several views of one object — an engineering
sheet with front, top, side and iso views, or an architectural sheet with a
plan and elevations — and press **Analyze**. The image is sent (downscaled to
2048 px on its long side) to the vision model set for *Image to model* in
`AISETTINGS`. Good choices are Claude, GPT or Gemini models on OpenRouter, or
`qwen2.5vl` / `llama3.2-vision` on Ollama.

The model returns each view's position on the sheet, its real size (read
from the dimension text), its outer silhouette and its visible and hidden
edges. The dialog draws all of it over the image so you can check it, and
lists any disagreement between views, such as a front width that differs
from the top width.

**Insert** adds every view on its own layer (`AI-FRONT`, `AI-TOP`,
`AI-RIGHT`, …, with hidden edges on `…-HIDDEN` in gray), keeping the
sheet's arrangement. The views go to the right of what is already in the
drawing and are converted into the drawing's units. With *Also build a 3D
solid* ticked, the front, top and side silhouettes are extruded through the
part, turned into their planes and intersected into one 3D solid next to the
views. This needs at least two of those three views, and holes and pockets
that do not show in the silhouettes are not cut. Everything inserted is one
undo step.

## Sketch to vector — `AISKETCH`

Choose a photo of a hand sketch or a scanned drawing and press **Analyze**.
The model traces it as one view with real sizes. With *Straighten…* ticked,
lines within 3° of horizontal or vertical are made exact and line ends closer
than 0.5 % of the drawing size are joined. Review the overlay, then
**Insert**. The model set for *Sketch to vector* in `AISETTINGS` is used.

Both commands share one window and can be switched at the top. Hosted models
show the estimated cost first when *Show a token and cost estimate* is on.

## Drawing checker — `AICHECK`

Choose a standard and press **Run checks**. The rules run locally — no model
is needed — and report:

| Rule | Finds |
| --- | --- |
| `DIM-VALUE` | Dimensions whose typed text shows a different number from the measured size (error) |
| `DIM-TEXT` | Dimensions whose measurement is replaced by text without `<>` |
| `DIM-ASSOC` | Model-space dimensions no longer associated with geometry (fix with `DIMREASSOCIATE`) |
| `LAYER-0` | Objects on layer 0 |
| `BYLAYER` | Objects that override their layer's color or linetype (fix with `SETBYLAYER`) |
| `LAYER-NAME`, `LAYER-MISSING` | Layer names that break the company pattern; required layers that are missing |
| `TEXT-HEIGHT` | Text heights outside the standard's series — ISO 3098 (1.8, 2.5, 3.5, 5, 7, 10, 14, 20 mm) or ASME Y14.2 (3 mm minimum, 6 mm titles); model-space text may be any of these times a common drawing scale |
| `TEXT-OVERLAP` | Text and dimensions that overlap other annotation in the same space |
| `DUPLICATE`, `ZERO-SIZE` | Exact duplicates on the same layer; zero-length lines and zero-radius circles |

**Show** selects and zooms to the objects of a finding (type `AICHECK` to
return to the list). **Erase** removes duplicates or zero-size objects as one
undo step and runs the checks again.

**AI review** sends the findings with a compact list of the dimensions, text
and geometry to the model set for *Drawing checker*. It looks for what rules
cannot judge — undimensioned or doubly dimensioned features, dimensions on
hidden lines, notes and title-block text that contradict each other — and
adds those as **AI** findings. Treat them as suggestions.

A company standard is a JSON file loaded with **Load standard file…** (kept
for later sessions); any field left out takes the ISO value:

```json
{
  "name": "ACME drafting standard",
  "text_heights_mm": [2.5, 3.5, 5, 7],
  "allow_layer_zero": false,
  "require_bylayer": true,
  "layer_pattern": "[ASMEC]-*|DEFPOINTS",
  "required_layers": ["A-TITLE", "A-ANNO-DIMS"],
  "require_associative_dimensions": true,
  "notes": "Hole positions are dimensioned from the lower-left datum."
}
```

Layer patterns use `*` (any text), `?` (one character), `#` (one digit),
`[ABC]` (one of) and `|` between alternatives, case-insensitively.

## Constraint suggestions — `AICONSTRAIN`

Select a sketch (lines, arcs, circles, 2D polylines; with nothing selected,
every such object in model space up to 200) and run `AICONSTRAIN`. The dialog
starts with the geometric constraints Auto Constrain finds. **Suggest** asks
the model set for *Constraint inference* for design intent — equal sizes
across a pattern, symmetry about a centre line, concentric features — and a
dimension scheme a drafter would use.

Every suggestion is checked against the geometry before it is listed: a
relation is kept only if it already holds within the Auto Constrain
tolerances, and a dimension always takes the measured size. Proposals that
fail are listed as dropped with the reason. Tick what you want and press
**Apply**: everything is added as one undo step. If the solver would still
have to move anything (conflicting constraints), the step is taken back and
nothing changes.

## Text to part — `AIPART`

Describe a part — "M8 hex bolt, 40 mm long", "flange Ø80 × 10 with a Ø40 hub
20 high, Ø10 bore and four M8 clearance holes on a 60 PCD", "L-bracket 3 mm
sheet, 50 × 50, two 6 mm holes per leg" — and press **Generate**. The model
set for *Text to part* writes a parametric definition: named sizes, and
bodies that are boxes, cylinders, regular prisms, extruded profiles or
revolved half-sections, with circular or linear patterns, joined (`add`),
removed (`cut`) or intersected. Threads are modelled at their nominal
diameter.

Each body points along Z unless it says otherwise: `"axis": "x"` or `"y"`
lays a cylinder, prism, extrusion or revolved section along that axis from
its position, and its pattern then goes round that axis. That is how pipes,
tees, cross holes and flanges on side branches are built — for example a
DN100 flanged tee with the run along X, the branch along Y and eight bolt
holes on each of the three flanges.

A linear pattern may step along the body's own axis with `dz` — a stack of
45 cooler fins is one body with `"linear": {"count": 45, "dz": 2.2}`.
Added bodies are joined one body (with all its pattern copies) at a time,
in the order written; if a join fails, putting small parts that meet a
curved surface earlier in the list often helps.

Review the parameters before anything is drawn. Each field takes a number or
an expression of other parameters (`D/2 + 5`), and sizes that depend on one
you change follow it. A value that would make a body impossible — a negative
diameter — is refused next to its field. **Generate again** with a new
request refines the current definition.

**Insert** builds the solid with the program's own EXTRUDE, REVOLVE, UNION,
SUBTRACT and INTERSECT on layer `AI-PART`, to the right of what is already
drawn, as one undo step. The definition is stored on the solid, so selecting
it and running `AIPART` again opens its parameters: change them and
**Rebuild** replaces the solid in place, again as one undo step.

## Floor-plan assistant — `AIPLAN`

`AIPLAN` gets a floor plan from one of three sources:

- **From a brief** — "3-bedroom bungalow, 12 × 9 m, open kitchen-living, 2
  bathrooms". The model set for *Floor plan assistant* lays out walls, doors,
  windows and rooms. **Generate again** with a change ("make the kitchen 1 m
  wider", "add a window to bedroom 2") refines the same plan.
- **From a plan image** — a scanned or exported floor plan. A vision model
  traces the walls, doors and windows and reads the room names and dimension
  strings. A PDF plan can be exported to an image first.
- **From selected lines** — no model needed: selected lines and open
  polylines become wall centre lines of the given thickness, and closed
  polylines become rooms.

The preview shows walls, doors (orange), windows (blue) and each room with
its area. Rename the rooms in the list beside it. The plan is checked before
it is shown: every opening must fit its wall and openings must not overlap.

**Insert** draws, as one undo step, on AIA-standard layers:

| Layer | Content |
| --- | --- |
| `A-WALL` | Solid wall poché, split around openings, with closed corners |
| `A-DOOR` | Door leaves and quarter-circle swings (a pair of leaves for openings of 1.5 m / 5 ft or more) |
| `A-GLAZ` | Window frames and glass lines |
| `A-AREA` | Closed room boundaries (usable with `AREA`, `MEASUREGEOM`) |
| `A-AREA-IDEN` | Room names and areas, centred and scaled down to fit narrow rooms |
| `A-ANNO-SCHD` | Room schedule: name, area, perimeter and total |
| `A-WALL-3D` | Optional 3D massing: wall solids to the storey height, with lintels over doors and wall above and below windows |

Plans from the model are in metres (or the units they name, such as feet)
and are converted into the drawing's units; areas are reported in m² — or
sq ft, with perimeters in feet and inches, in an inch or foot drawing —
whenever the drawing's units are known.

## Drawing views from a solid — `AIVIEWS`

Select one 3D solid and run `AIVIEWS`. Front, top, right and isometric views
are projected from the solid's exact edges and its smooth-surface outlines,
with hidden-line removal: every edge is cut into short pieces that are tested
against the solid toward the viewer. Hidden pieces that lie under visible
lines are dropped, tangent edges between smoothly joined faces are left out,
and circular edges come out as true circles and arcs. Untick views you do
not want, or untick *Hidden lines*.

The preview shows the sheet laid out third-angle: the top view above the
front, the right view beside it and the isometric view at the upper right.
**Insert** places it to the right of the drawing as one undo step, on these
layers:

| Layer | Content |
| --- | --- |
| `AI-VIEW` | Visible edges and outlines |
| `AI-VIEW-HIDDEN` | Hidden edges (gray, `HIDDEN` linetype) |
| `AI-VIEW-CENTER` | Centre marks on circles (`CENTER` linetype) |
| `AI-VIEW-LABEL` | View names |
| `AI-VIEW-DIMS` | Dimensions |

*Dimensions*:

- **Basic** — overall width and height on the front view, depth on the top
  view, each hole size once (`DIMDIAMETER` on the view's circles) and each
  hole centre located from the lower-left corner of the view where holes
  show as circles.
- **AI dimension scheme** — the model set for *Auto dimensioning* is given
  the views' corner points and circles and chooses a complete,
  non-redundant scheme to ISO 129-1 or ASME Y14.5. It picks *which* points
  and circles to dimension; the values are always measured from the views.
- **No dimensions**.

Dimensions use the `AI-VIEWS` dimension style: a copy of the current style
whose DIMSCALE sizes the text in proportion to the views (and, for ASME
Y14.5, a decimal point). The current style is left as it was. Linetype
scales of hidden and centre lines also follow the size of the views.
Diameter leaders point away from the view's centre and fan out for
concentric circles.
