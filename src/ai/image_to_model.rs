//! Image to model (AIIMAGE) and sketch to vector (AISKETCH).
//!
//! A vision model reads an image — a sheet with several orthographic views
//! of a part or building, or a single hand sketch / scanned drawing — and
//! answers with a structured description: each view's position in the image
//! (pixels, for the review overlay), its size in drawing units, its outer
//! silhouette and its visible / hidden geometry in view-local coordinates.
//!
//! Everything here is pure: parsing and checking the reading, cleaning up
//! sketch geometry, laying the views out, and turning the result into the
//! automation operations that [`super::command_plan::Plan`] applies — 2D
//! views with `entities_create`, and optionally a 3D solid by extruding the
//! front, top and right silhouettes and intersecting them.

use serde_json::{json, Value};

pub const MAX_VIEWS: usize = 12;
pub const MAX_SHAPES: usize = 4000;
/// Images are downscaled to this many pixels on the long side before sending.
pub const MAX_IMAGE_SIDE: u32 = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Part,
    Building,
}

impl Subject {
    pub const ALL: [Subject; 2] = [Subject::Part, Subject::Building];
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Subject::Part => "Mechanical part",
            Subject::Building => "Building",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Several orthographic views of one object (AIIMAGE).
    MultiView,
    /// One drawing or sketch to trace (AISKETCH).
    Sketch,
}

/// Where a view looks from. Buildings call the top view a plan and the side
/// views elevations; both map onto the same kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Front,
    Top,
    Right,
    Left,
    Back,
    Bottom,
    Iso,
    Section,
    Detail,
    Other,
}

impl ViewKind {
    fn parse(s: &str) -> Self {
        let s = s.trim().to_ascii_lowercase();
        let s = s.replace(['_', '-'], " ");
        match s.as_str() {
            "front" | "front elevation" | "south elevation" | "elevation" => ViewKind::Front,
            "top" | "plan" | "floor plan" | "roof plan" | "site plan" => ViewKind::Top,
            "right" | "right side" | "right elevation" | "east elevation" | "side" => {
                ViewKind::Right
            }
            "left" | "left side" | "left elevation" | "west elevation" => ViewKind::Left,
            "back" | "rear" | "rear elevation" | "north elevation" => ViewKind::Back,
            "bottom" => ViewKind::Bottom,
            "iso" | "isometric" | "axonometric" | "perspective" | "3d" => ViewKind::Iso,
            "section" => ViewKind::Section,
            "detail" => ViewKind::Detail,
            _ if s.contains("section") => ViewKind::Section,
            _ if s.contains("iso") => ViewKind::Iso,
            _ => ViewKind::Other,
        }
    }

    pub fn layer(self) -> &'static str {
        match self {
            ViewKind::Front => "AI-FRONT",
            ViewKind::Top => "AI-TOP",
            ViewKind::Right => "AI-RIGHT",
            ViewKind::Left => "AI-LEFT",
            ViewKind::Back => "AI-BACK",
            ViewKind::Bottom => "AI-BOTTOM",
            ViewKind::Iso => "AI-ISO",
            ViewKind::Section => "AI-SECTION",
            ViewKind::Detail => "AI-DETAIL",
            ViewKind::Other => "AI-VIEW",
        }
    }
}

/// One shape in view-local drawing units (origin at the view's lower-left
/// corner, Y up).
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Line {
        a: [f64; 2],
        b: [f64; 2],
        hidden: bool,
    },
    Circle {
        c: [f64; 2],
        r: f64,
        hidden: bool,
    },
    Arc {
        c: [f64; 2],
        r: f64,
        start: f64,
        end: f64,
        hidden: bool,
    },
    Polyline {
        pts: Vec<[f64; 2]>,
        closed: bool,
        hidden: bool,
    },
    Text {
        at: [f64; 2],
        height: f64,
        value: String,
    },
}

impl Shape {
    pub fn hidden(&self) -> bool {
        match self {
            Shape::Line { hidden, .. }
            | Shape::Circle { hidden, .. }
            | Shape::Arc { hidden, .. }
            | Shape::Polyline { hidden, .. } => *hidden,
            Shape::Text { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub name: String,
    pub kind: ViewKind,
    /// `[x0, y0, x1, y1]` in image pixels, y down.
    pub bbox_px: [f64; 4],
    /// Size in drawing units.
    pub width: f64,
    pub height: f64,
    /// Outer silhouette, closed, view-local units.
    pub outline: Vec<[f64; 2]>,
    pub shapes: Vec<Shape>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub units: String,
    pub views: Vec<View>,
    pub notes: String,
}

// ── Prompt ──────────────────────────────────────────────────────────────────

pub fn system_prompt(
    task: Task,
    subject: Subject,
    units_hint: &str,
    width_px: u32,
    height_px: u32,
) -> String {
    let what = match (task, subject) {
        (Task::MultiView, Subject::Part) => "an engineering drawing sheet showing several orthographic views (front, top, right/left side, possibly an isometric) of ONE mechanical part",
        (Task::MultiView, Subject::Building) => "an architectural sheet showing a floor plan and elevations (and possibly a 3D view) of ONE building",
        (Task::Sketch, Subject::Part) => "ONE hand sketch or scanned technical drawing of a part",
        (Task::Sketch, Subject::Building) => "ONE hand sketch or scanned plan/elevation of a building",
    };
    let views_rule = match task {
        Task::MultiView => "List every view you can see. Give each its kind: front, top (plan), right, left, back, bottom, iso, section or detail. For third-angle sheets the top view is above the front view and the right view is to its right; for first-angle sheets they are swapped — use the projection symbol or the geometry to decide, and name the views by what they show, not where they sit.",
        Task::Sketch => "Treat the whole drawing as ONE view (kind \"front\" for an elevation or part view, \"top\" for a plan). Straighten lines that are meant to be straight, make near-right angles exact, close shapes that are meant to be closed, and turn roughly round shapes into circles and arcs.",
    };
    format!(
        r#"You convert drawings into CAD geometry. The image ({width_px}×{height_px} pixels) is {what}.

{views_rule}

Read the dimension text to get real sizes. Use units "{units_hint}" unless the drawing clearly states others (then say which in "units"). If there are no dimensions, estimate sensible real sizes for the subject and say so in "notes".

Answer with ONE JSON object and nothing else:
{{
  "units": "mm" | "cm" | "m" | "in" | "ft",
  "notes": "<assumptions, unreadable parts, projection angle>",
  "views": [
    {{
      "name": "<label as written, or your own>",
      "kind": "front" | "top" | "right" | "left" | "back" | "bottom" | "iso" | "section" | "detail",
      "bbox_px": [x0, y0, x1, y1],          // the view's geometry in the image, pixels, origin top-left, y down
      "width": <real width of the view>,     // drawing units
      "height": <real height of the view>,   // drawing units
      "outline": [[x, y], ...],              // outer silhouette, closed polygon, view-local units
      "entities": [
        {{"type": "line", "a": [x, y], "b": [x, y], "hidden": false}},
        {{"type": "circle", "c": [x, y], "r": r}},
        {{"type": "arc", "c": [x, y], "r": r, "start": deg, "end": deg}},
        {{"type": "polyline", "pts": [[x, y], ...], "closed": true}},
        {{"type": "text", "at": [x, y], "height": h, "value": "..."}}
      ]
    }}
  ]
}}

Coordinates inside a view are in drawing units with the origin at the LOWER-LEFT corner of that view's geometry, x to the right, y UP (0 ≤ x ≤ width, 0 ≤ y ≤ height). Angles are degrees counterclockwise from +x. Mark hidden (dashed) edges "hidden": true. Do not draw dimension lines, arrows, borders or title blocks as geometry; put labels that belong to the object (room names, notes on the part) in text entities only when useful. For an iso view give only bbox_px, width, height and an empty entity list unless asked otherwise. Views of the same object must agree: front width = top width, front height = side height, top depth = side width."#
    )
}

// ── Parsing ─────────────────────────────────────────────────────────────────

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|x: &f64| x.is_finite())
}

fn pt(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array()?;
    Some([num(a.first()?)?, num(a.get(1)?)?])
}

fn pts(v: &Value) -> Vec<[f64; 2]> {
    v.as_array()
        .map(|a| a.iter().filter_map(pt).collect())
        .unwrap_or_default()
}

fn shape(v: &Value) -> Option<Shape> {
    let hidden = v["hidden"].as_bool().unwrap_or(false);
    match v["type"].as_str()?.to_ascii_lowercase().as_str() {
        "line" => Some(Shape::Line {
            a: pt(&v["a"]).or_else(|| pt(&v["start"]))?,
            b: pt(&v["b"]).or_else(|| pt(&v["end"]))?,
            hidden,
        }),
        "circle" => {
            let r = num(&v["r"]).or_else(|| num(&v["radius"]))?;
            (r > 0.0).then_some(Shape::Circle {
                c: pt(&v["c"]).or_else(|| pt(&v["center"]))?,
                r,
                hidden,
            })
        }
        "arc" => {
            let r = num(&v["r"]).or_else(|| num(&v["radius"]))?;
            (r > 0.0).then_some(Shape::Arc {
                c: pt(&v["c"]).or_else(|| pt(&v["center"]))?,
                r,
                start: num(&v["start"]).or_else(|| num(&v["start_angle_deg"]))?,
                end: num(&v["end"]).or_else(|| num(&v["end_angle_deg"]))?,
                hidden,
            })
        }
        "polyline" | "lwpolyline" | "polygon" => {
            let p = pts(if v["pts"].is_array() {
                &v["pts"]
            } else {
                &v["vertices"]
            });
            (p.len() >= 2).then_some(Shape::Polyline {
                closed: v["closed"].as_bool().unwrap_or(v["type"] == "polygon"),
                pts: p,
                hidden,
            })
        }
        "text" => {
            let value = v["value"].as_str()?.trim().to_string();
            (!value.is_empty()).then_some(Shape::Text {
                at: pt(&v["at"]).or_else(|| pt(&v["position"]))?,
                height: num(&v["height"]).filter(|h| *h > 0.0).unwrap_or(2.5),
                value,
            })
        }
        _ => None,
    }
}

/// Parse and sanity-check a model reply.
pub fn parse_reading(reply: &str) -> Result<Reading, String> {
    let v = super::client::extract_json(reply)
        .ok_or_else(|| "the model did not answer with JSON".to_string())?;
    let views_json = v["views"]
        .as_array()
        .ok_or("the reply has no \"views\" list")?;
    if views_json.is_empty() {
        return Err(format!(
            "no views were recognised{}",
            v["notes"]
                .as_str()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default()
        ));
    }
    if views_json.len() > MAX_VIEWS {
        return Err(format!(
            "{} views is more than the {MAX_VIEWS} allowed",
            views_json.len()
        ));
    }
    let mut views = Vec::new();
    let mut total = 0;
    for (n, vj) in views_json.iter().enumerate() {
        let bbox = vj["bbox_px"]
            .as_array()
            .filter(|a| a.len() == 4)
            .and_then(|a| Some([num(&a[0])?, num(&a[1])?, num(&a[2])?, num(&a[3])?]))
            .ok_or_else(|| format!("view {}: \"bbox_px\" must be [x0, y0, x1, y1]", n + 1))?;
        let bbox = [
            bbox[0].min(bbox[2]),
            bbox[1].min(bbox[3]),
            bbox[0].max(bbox[2]),
            bbox[1].max(bbox[3]),
        ];
        let width = num(&vj["width"]).unwrap_or(0.0);
        let height = num(&vj["height"]).unwrap_or(0.0);
        if !(width > 0.0 && height > 0.0) {
            return Err(format!("view {}: width and height must be positive", n + 1));
        }
        let shapes: Vec<Shape> = vj["entities"]
            .as_array()
            .map(|a| a.iter().filter_map(shape).collect())
            .unwrap_or_default();
        total += shapes.len();
        if total > MAX_SHAPES {
            return Err(format!(
                "more than {MAX_SHAPES} shapes — try a simpler or cropped image"
            ));
        }
        let mut outline = pts(&vj["outline"]);
        if outline.len() >= 2 && outline.first() == outline.last() {
            outline.pop();
        }
        if outline.len() < 3 {
            outline = vec![[0.0, 0.0], [width, 0.0], [width, height], [0.0, height]];
        }
        views.push(View {
            name: vj["name"].as_str().unwrap_or("").trim().to_string(),
            kind: ViewKind::parse(vj["kind"].as_str().unwrap_or("")),
            bbox_px: bbox,
            width,
            height,
            outline,
            shapes,
        });
    }
    Ok(Reading {
        units: v["units"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase(),
        notes: v["notes"].as_str().unwrap_or("").trim().to_string(),
        views,
    })
}

// ── Checks and cleanup ──────────────────────────────────────────────────────

fn view_of(reading: &Reading, kind: ViewKind) -> Option<&View> {
    reading.views.iter().find(|v| v.kind == kind)
}

/// The side view used for depth: right, else left.
fn side_view(reading: &Reading) -> Option<&View> {
    view_of(reading, ViewKind::Right).or_else(|| view_of(reading, ViewKind::Left))
}

/// Disagreements between views that share a dimension (2 % tolerance).
pub fn consistency_warnings(reading: &Reading) -> Vec<String> {
    let mut out = Vec::new();
    let close = |a: f64, b: f64| (a - b).abs() <= 0.02 * a.abs().max(b.abs()).max(1e-9);
    let front = view_of(reading, ViewKind::Front);
    let top = view_of(reading, ViewKind::Top);
    let side = side_view(reading);
    if let (Some(f), Some(t)) = (front, top) {
        if !close(f.width, t.width) {
            out.push(format!(
                "front width {} ≠ top width {}",
                fmt(f.width),
                fmt(t.width)
            ));
        }
    }
    if let (Some(f), Some(s)) = (front, side) {
        if !close(f.height, s.height) {
            out.push(format!(
                "front height {} ≠ side height {}",
                fmt(f.height),
                fmt(s.height)
            ));
        }
    }
    if let (Some(t), Some(s)) = (top, side) {
        if !close(t.height, s.width) {
            out.push(format!(
                "top depth {} ≠ side width {}",
                fmt(t.height),
                fmt(s.width)
            ));
        }
    }
    for v in &reading.views {
        let slack = 0.05 * v.width.max(v.height);
        let outside = v
            .shapes
            .iter()
            .filter(|s| !shape_inside(s, v.width, v.height, slack))
            .count();
        if outside > 0 {
            out.push(format!(
                "{}: {outside} shape(s) fall outside the view's {} × {} box",
                view_label(v),
                fmt(v.width),
                fmt(v.height)
            ));
        }
    }
    out
}

fn shape_inside(s: &Shape, w: f64, h: f64, slack: f64) -> bool {
    let inside =
        |p: [f64; 2]| p[0] >= -slack && p[1] >= -slack && p[0] <= w + slack && p[1] <= h + slack;
    match s {
        Shape::Line { a, b, .. } => inside(*a) && inside(*b),
        Shape::Circle { c, r, .. } | Shape::Arc { c, r, .. } => {
            inside([c[0] - r, c[1] - r]) && inside([c[0] + r, c[1] + r])
        }
        Shape::Polyline { pts, .. } => pts.iter().all(|p| inside(*p)),
        Shape::Text { at, .. } => inside(*at),
    }
}

pub fn view_label(v: &View) -> String {
    if v.name.is_empty() {
        format!("{:?} view", v.kind)
    } else {
        v.name.clone()
    }
}

fn fmt(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Sketch cleanup: lines within `angle_tol_deg` of horizontal / vertical
/// are made exact, and line endpoints closer than `weld` are joined.
pub fn tidy_sketch(view: &mut View, angle_tol_deg: f64, weld: f64) {
    let tol = angle_tol_deg.to_radians();
    for s in view.shapes.iter_mut() {
        if let Shape::Line { a, b, .. } = s {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let ang = dy.atan2(dx).abs();
            if ang < tol || (std::f64::consts::PI - ang) < tol {
                let y = (a[1] + b[1]) / 2.0;
                a[1] = y;
                b[1] = y;
            } else if (ang - std::f64::consts::FRAC_PI_2).abs() < tol {
                let x = (a[0] + b[0]) / 2.0;
                a[0] = x;
                b[0] = x;
            }
        }
    }
    // Weld by clustering coordinates: x values (and separately y values)
    // closer than `weld` take their cluster's mean. Endpoints that nearly
    // meet then meet exactly, and lines straightened above stay straight
    // because both of their shared coordinates land in the same cluster.
    for axis in 0..2 {
        let mut values: Vec<f64> = Vec::new();
        for s in view.shapes.iter() {
            match s {
                Shape::Line { a, b, .. } => values.extend([a[axis], b[axis]]),
                Shape::Polyline { pts, .. } => values.extend(pts.iter().map(|p| p[axis])),
                _ => {}
            }
        }
        values.sort_by(f64::total_cmp);
        let mut clusters: Vec<(f64, f64, f64)> = Vec::new(); // (lo, hi, mean)
        let mut i = 0;
        while i < values.len() {
            let mut j = i + 1;
            while j < values.len() && values[j] - values[j - 1] <= weld {
                j += 1;
            }
            let mean = values[i..j].iter().sum::<f64>() / (j - i) as f64;
            clusters.push((values[i], values[j - 1], mean));
            i = j;
        }
        let snap = |x: &mut f64| {
            if let Some(c) = clusters.iter().find(|c| *x >= c.0 && *x <= c.1) {
                *x = c.2;
            }
        };
        for s in view.shapes.iter_mut() {
            match s {
                Shape::Line { a, b, .. } => {
                    snap(&mut a[axis]);
                    snap(&mut b[axis]);
                }
                Shape::Polyline { pts, .. } => pts.iter_mut().for_each(|p| snap(&mut p[axis])),
                _ => {}
            }
        }
    }
    view.shapes.retain(|s| match s {
        Shape::Line { a, b, .. } => (a[0] - b[0]).hypot(a[1] - b[1]) > 1e-9,
        _ => true,
    });
}

/// Millimetres per unit, for converting the reading into the drawing's units.
pub fn mm_per_unit(name: &str) -> Option<f64> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "mm" | "millimeter" | "millimeters" | "millimetre" | "millimetres" => 1.0,
        "cm" | "centimeter" | "centimeters" => 10.0,
        "dm" | "decimeters" => 100.0,
        "m" | "meter" | "meters" | "metre" | "metres" => 1000.0,
        "in" | "inch" | "inches" | "\"" => 25.4,
        "ft" | "foot" | "feet" | "'" => 304.8,
        "yd" | "yards" => 914.4,
        _ => return None,
    })
}

/// Scale every coordinate and size by `k`.
pub fn scale_reading(reading: &mut Reading, k: f64) {
    if (k - 1.0).abs() < 1e-12 {
        return;
    }
    let sp = |p: &mut [f64; 2]| {
        p[0] *= k;
        p[1] *= k;
    };
    for v in reading.views.iter_mut() {
        v.width *= k;
        v.height *= k;
        v.outline.iter_mut().for_each(sp);
        for s in v.shapes.iter_mut() {
            match s {
                Shape::Line { a, b, .. } => {
                    sp(a);
                    sp(b);
                }
                Shape::Circle { c, r, .. } | Shape::Arc { c, r, .. } => {
                    sp(c);
                    *r *= k;
                }
                Shape::Polyline { pts, .. } => pts.iter_mut().for_each(sp),
                Shape::Text { at, height, .. } => {
                    sp(at);
                    *height *= k;
                }
            }
        }
    }
}

// ── Layout and output ───────────────────────────────────────────────────────

/// World offset of each view's lower-left corner. Views keep their relative
/// placement on the sheet, measured in the front view's scale (the first
/// view's when there is no front view), starting at `origin`.
pub fn layout(reading: &Reading, origin: [f64; 2]) -> Vec<[f64; 2]> {
    let Some(anchor) = view_of(reading, ViewKind::Front).or_else(|| reading.views.first()) else {
        return Vec::new();
    };
    let px_w = (anchor.bbox_px[2] - anchor.bbox_px[0]).max(1.0);
    let k = anchor.width / px_w;
    reading
        .views
        .iter()
        .map(|v| {
            [
                origin[0] + (v.bbox_px[0] - anchor.bbox_px[0]) * k,
                origin[1] + (anchor.bbox_px[3] - v.bbox_px[3]) * k,
            ]
        })
        .collect()
}

fn add(p: [f64; 2], o: [f64; 2]) -> [f64; 2] {
    [p[0] + o[0], p[1] + o[1]]
}

/// `entities_create` definitions for one view placed at `offset`.
pub fn view_entities(view: &View, offset: [f64; 2]) -> Vec<Value> {
    let layer = view.kind.layer();
    let hidden_layer = format!("{layer}-HIDDEN");
    let text_layer = format!("{layer}-TEXT");
    let mut out = Vec::new();
    for s in &view.shapes {
        let lay = if s.hidden() {
            hidden_layer.as_str()
        } else {
            layer
        };
        let color = if s.hidden() { json!(8) } else { json!(256) };
        out.push(match s {
            Shape::Line { a, b, .. } => json!({"type":"Line","start":add(*a, offset),"end":add(*b, offset),"layer":lay,"color":color}),
            Shape::Circle { c, r, .. } => json!({"type":"Circle","center":add(*c, offset),"radius":r,"layer":lay,"color":color}),
            Shape::Arc { c, r, start, end, .. } => json!({"type":"Arc","center":add(*c, offset),"radius":r,"start_angle_deg":start,"end_angle_deg":end,"layer":lay,"color":color}),
            Shape::Polyline { pts, closed, .. } => json!({"type":"LwPolyline","vertices":pts.iter().map(|p| add(*p, offset)).collect::<Vec<_>>(),"closed":closed,"layer":lay,"color":color}),
            Shape::Text { at, height, value } => json!({"type":"Text","value":value,"position":add(*at, offset),"height":height,"layer":text_layer}),
        });
    }
    // A view the model gave no edges for (an iso view) still gets its frame
    // so the placement is visible.
    if view.shapes.is_empty() {
        out.push(json!({
            "type":"LwPolyline",
            "vertices": view.outline.iter().map(|p| add(*p, offset)).collect::<Vec<_>>(),
            "closed": true, "layer": layer
        }));
    }
    out
}

/// The 2D steps: one `entities_create` per view.
pub fn plan_2d(reading: &Reading, origin: [f64; 2]) -> Vec<Value> {
    let offsets = layout(reading, origin);
    reading
        .views
        .iter()
        .zip(offsets)
        .filter(|(v, _)| !view_entities(v, [0.0, 0.0]).is_empty())
        .map(|(v, o)| {
            json!({
                "op": "entities_create",
                "entities": view_entities(v, o),
                "note": format!("{} ({} × {})", view_label(v), fmt(v.width), fmt(v.height)),
            })
        })
        .collect()
}

/// Steps that build a 3D solid from the front, top and side silhouettes:
/// each closed outline is drawn, extruded through the part, turned into its
/// plane and moved into place; the solids are then intersected. World axes:
/// X width, Y depth, Z height, with the solid's corner at `origin` (z = 0).
///
/// Uses the interactive `start` / `input` ops with named bindings, so it is
/// built here rather than written by the model.
pub fn plan_3d(reading: &Reading, origin: [f64; 2]) -> Result<Vec<Value>, String> {
    let front = view_of(reading, ViewKind::Front);
    let top = view_of(reading, ViewKind::Top);
    let side = side_view(reading);
    let views = [front, top, side].iter().filter(|v| v.is_some()).count();
    if views < 2 {
        return Err("a 3D solid needs at least two of the front, top and side views".into());
    }
    let w = front.or(top).map(|v| v.width).unwrap_or(0.0);
    let h = front.or(side).map(|v| v.height).unwrap_or(0.0);
    let d = top
        .map(|v| v.height)
        .or_else(|| side.map(|v| v.width))
        .unwrap_or(0.0);
    if !(w > 0.0 && h > 0.0 && d > 0.0) {
        return Err("the views do not give a width, depth and height".into());
    }
    let [ox, oy] = origin;
    let mut steps = Vec::new();
    let mut solids = Vec::new();

    // A profile drawn flat at the origin, extruded by `depth` along +Z.
    let extrude = |steps: &mut Vec<Value>, name: &str, outline: &[[f64; 2]], depth: f64| {
        steps.push(json!({"op":"entities_create","entities":[{"type":"LwPolyline","vertices":outline,"closed":true,"layer":"AI-3D"}],"bind":format!("{name}_profile"),"note":format!("{name} silhouette")}));
        steps.push(json!({"op":"start","cmd":"EXTRUDE"}));
        steps.push(json!({"op":"input","kind":"selection","handles":format!("${name}_profile")}));
        steps.push(json!({"op":"input","kind":"enter"}));
        steps.push(json!({"op":"input","kind":"text","text":fmt(depth),"bind":name.to_string(),"note":format!("extrude {name} by {}", fmt(depth))}));
    };
    let rotate = |steps: &mut Vec<Value>, name: &str, axis: &str, angle: f64| {
        steps.push(json!({"op":"start","cmd":"3DROTATE"}));
        steps.push(json!({"op":"input","kind":"selection","handles":format!("${name}")}));
        steps.push(json!({"op":"input","kind":"enter"}));
        steps.push(json!({"op":"input","kind":"point","point":[0.0,0.0,0.0]}));
        steps.push(json!({"op":"input","kind":"token","text":axis}));
        steps.push(json!({"op":"input","kind":"text","text":fmt(angle),"note":format!("turn {name} {}° about {axis}", fmt(angle))}));
    };
    let place = |steps: &mut Vec<Value>, name: &str, v: [f64; 3]| {
        steps.push(json!({"op":"entities_transform","handles":format!("${name}"),"action":"move","vector":v,"note":format!("place {name}")}));
    };

    if let Some(t) = top {
        // Plan: x → X, y → Y; extruded up through the height.
        extrude(&mut steps, "top", &t.outline, h);
        place(&mut steps, "top", [ox, oy, 0.0]);
        solids.push("top");
    }
    if let Some(f) = front {
        // Front: x → X, y → Z, extruded through the depth. Drawn in XY and
        // extruded along +Z, a +90° turn about X maps (x, y, z) to
        // (x, -z, y); moving +depth in Y brings it to 0 ≤ Y ≤ depth.
        extrude(&mut steps, "front", &f.outline, d);
        rotate(&mut steps, "front", "X", 90.0);
        place(&mut steps, "front", [ox, oy + d, 0.0]);
        solids.push("front");
    }
    if let Some(s) = side {
        // Right side seen from +X: x → Y, y → Z, extruded through the width.
        // +90° about Z then +90° about Y maps (x, y, z) to (z, x, y). A left
        // view is the mirror image, so its x runs the other way.
        let outline: Vec<[f64; 2]> = if s.kind == ViewKind::Left {
            s.outline.iter().map(|p| [s.width - p[0], p[1]]).collect()
        } else {
            s.outline.clone()
        };
        extrude(&mut steps, "side", &outline, w);
        rotate(&mut steps, "side", "Z", 90.0);
        rotate(&mut steps, "side", "Y", 90.0);
        place(&mut steps, "side", [ox, oy, 0.0]);
        solids.push("side");
    }
    steps.push(json!({"op":"start","cmd":"INTERSECT"}));
    steps.push(json!({"op":"input","kind":"selection","handles":solids.iter().map(|s| format!("${s}")).collect::<Vec<_>>()}));
    steps.push(
        json!({"op":"input","kind":"enter","note":"intersect the silhouettes into one solid"}),
    );
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"units":"mm","notes":"third angle","views":[
        {"name":"FRONT","kind":"front","bbox_px":[100,500,500,700],"width":80,"height":40,
         "outline":[[0,0],[80,0],[80,40],[0,40]],
         "entities":[{"type":"line","a":[0,0],"b":[80,0]},{"type":"circle","c":[40,20],"r":5,"hidden":true}]},
        {"name":"TOP","kind":"plan","bbox_px":[100,200,500,450],"width":80,"height":50,
         "outline":[[0,0],[80,0],[80,50],[0,50]],"entities":[{"type":"polyline","pts":[[0,0],[80,0],[80,50]],"closed":false}]},
        {"name":"RIGHT","kind":"right side","bbox_px":[550,500,800,700],"width":50,"height":40,
         "outline":[[0,0],[50,0],[50,40],[0,40],[0,0]],"entities":[]},
        {"name":"ISO","kind":"isometric","bbox_px":[600,100,900,400],"width":90,"height":90,"entities":[]}
    ]}"#;

    #[test]
    fn parses_views_and_kinds() {
        let r = parse_reading(SAMPLE).unwrap();
        assert_eq!(r.views.len(), 4);
        assert_eq!(r.views[1].kind, ViewKind::Top);
        assert_eq!(r.views[2].kind, ViewKind::Right);
        assert_eq!(r.views[2].outline.len(), 4, "closing point dropped");
        assert_eq!(r.views[3].kind, ViewKind::Iso);
        assert_eq!(
            r.views[3].outline.len(),
            4,
            "missing outline becomes the box"
        );
        assert!(r.views[0].shapes[1].hidden());
        assert!(
            consistency_warnings(&r).is_empty(),
            "{:?}",
            consistency_warnings(&r)
        );
    }

    #[test]
    fn rejects_bad_replies() {
        assert!(parse_reading("no json").is_err());
        assert!(parse_reading(r#"{"views":[]}"#).is_err());
        assert!(parse_reading(
            r#"{"views":[{"kind":"front","bbox_px":[0,0,1,1],"width":0,"height":1}]}"#
        )
        .is_err());
    }

    #[test]
    fn warns_when_views_disagree() {
        let mut r = parse_reading(SAMPLE).unwrap();
        r.views[1].width = 90.0;
        let w = consistency_warnings(&r);
        assert!(w.iter().any(|m| m.contains("front width 80")), "{w:?}");
    }

    #[test]
    fn layout_keeps_sheet_positions_in_front_scale() {
        let r = parse_reading(SAMPLE).unwrap();
        let o = layout(&r, [0.0, 0.0]);
        // 400 px = 80 mm → 0.2 mm/px. Front at origin.
        assert_eq!(o[0], [0.0, 0.0]);
        // Top view's bottom edge is 250 px above the front's bottom → 50 mm.
        assert!((o[1][1] - 50.0).abs() < 1e-9 && o[1][0].abs() < 1e-9);
        // Right view starts 450 px right → 90 mm.
        assert!((o[2][0] - 90.0).abs() < 1e-9);
    }

    #[test]
    fn view_entities_use_view_layers() {
        let r = parse_reading(SAMPLE).unwrap();
        let e = view_entities(&r.views[0], [10.0, 0.0]);
        assert_eq!(e[0]["layer"], "AI-FRONT");
        assert_eq!(e[0]["start"], json!([10.0, 0.0]));
        assert_eq!(e[1]["layer"], "AI-FRONT-HIDDEN");
        let iso = view_entities(&r.views[3], [0.0, 0.0]);
        assert_eq!(iso.len(), 1, "an empty view gets its frame");
        let steps = plan_2d(&r, [0.0, 0.0]);
        assert_eq!(steps.len(), 4);
        // The 2D steps pass the AICMD plan checks.
        let plan = json!({"summary":"x","steps":steps}).to_string();
        assert!(crate::ai::command_plan::parse_plan(&plan).is_ok());
    }

    #[test]
    fn plan_3d_needs_two_views_and_binds_names() {
        let r = parse_reading(SAMPLE).unwrap();
        let steps = plan_3d(&r, [0.0, 0.0]).unwrap();
        let last = steps.last().unwrap();
        assert_eq!(last["kind"], "enter");
        let sel = &steps[steps.len() - 2]["handles"];
        assert_eq!(sel, &json!(["$top", "$front", "$side"]));
        let mut only = r.clone();
        only.views.retain(|v| v.kind == ViewKind::Front);
        assert!(plan_3d(&only, [0.0, 0.0]).is_err());
    }

    #[test]
    fn tidy_straightens_and_welds() {
        let mut v = View {
            name: String::new(),
            kind: ViewKind::Front,
            bbox_px: [0.0; 4],
            width: 10.0,
            height: 10.0,
            outline: vec![],
            shapes: vec![
                Shape::Line {
                    a: [0.0, 0.0],
                    b: [10.0, 0.2],
                    hidden: false,
                },
                Shape::Line {
                    a: [10.1, 0.15],
                    b: [10.0, 10.0],
                    hidden: false,
                },
            ],
        };
        tidy_sketch(&mut v, 3.0, 0.3);
        let Shape::Line { a, b, .. } = &v.shapes[0] else {
            panic!()
        };
        assert_eq!(a[1], b[1], "horizontal made exact");
        let Shape::Line { a: a2, b: b2, .. } = &v.shapes[1] else {
            panic!()
        };
        assert_eq!(a2, b, "endpoints welded");
        assert_eq!(a2[0], b2[0], "vertical made exact");
    }

    #[test]
    fn unit_conversion() {
        assert_eq!(mm_per_unit("in"), Some(25.4));
        assert_eq!(mm_per_unit("Feet"), Some(304.8));
        let mut r = parse_reading(SAMPLE).unwrap();
        scale_reading(&mut r, 2.0);
        assert_eq!(r.views[0].width, 160.0);
        assert!(matches!(r.views[0].shapes[1], Shape::Circle { r, .. } if r == 10.0));
    }
}
