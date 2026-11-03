//! Drawing views from a 3D solid (AIVIEWS): orthographic front / top / right
//! views and an isometric view with hidden-line removal, centre marks and a
//! first-pass dimension scheme, laid out third-angle next to the solid.
//!
//! Input is the solid's tessellation: its triangles, its exact topological
//! edges (as polylines) and, per view, the smooth-face silhouette segments
//! the kernel computes. Each edge is cut into short pieces whose midpoints
//! are ray-cast toward the viewer against the triangles: a piece is hidden
//! when anything lies in front of it. Hidden pieces that coincide with
//! visible lines are dropped (visible wins), circular runs become circles
//! and arcs, and straight runs become lines.

use serde_json::{json, Value};

pub const L_VISIBLE: &str = "AI-VIEW";
pub const L_HIDDEN: &str = "AI-VIEW-HIDDEN";
pub const L_CENTER: &str = "AI-VIEW-CENTER";
pub const L_DIMS: &str = "AI-VIEW-DIMS";
pub const L_LABEL: &str = "AI-VIEW-LABEL";

type P3 = [f64; 3];
type P2 = [f64; 2];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewKind {
    Front,
    Top,
    Right,
    Iso,
}

impl ViewKind {
    pub const ALL: [ViewKind; 4] = [
        ViewKind::Front,
        ViewKind::Top,
        ViewKind::Right,
        ViewKind::Iso,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ViewKind::Front => "FRONT",
            ViewKind::Top => "TOP",
            ViewKind::Right => "RIGHT",
            ViewKind::Iso => "ISOMETRIC",
        }
    }

    /// The direction the viewer looks in, and the view's right and up axes
    /// (third-angle: front from −Y, top from +Z, right side from +X; the
    /// isometric view from the front-right-top corner).
    pub fn frame(self) -> (P3, P3, P3) {
        match self {
            ViewKind::Front => ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            ViewKind::Top => ([0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            ViewKind::Right => ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
            ViewKind::Iso => {
                let s3 = 3f64.sqrt();
                let s2 = 2f64.sqrt();
                let s6 = 6f64.sqrt();
                (
                    [-1.0 / s3, 1.0 / s3, -1.0 / s3],
                    [1.0 / s2, 1.0 / s2, 0.0],
                    [-1.0 / s6, 1.0 / s6, 2.0 / s6],
                )
            }
        }
    }
}

fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn sub3(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn lerp3(a: P3, b: P3, t: f64) -> P3 {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}
fn len3(a: P3) -> f64 {
    dot(a, a).sqrt()
}
fn d2(a: P2, b: P2) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// The solid as the views need it.
#[derive(Debug, Clone, Default)]
pub struct SolidData {
    pub triangles: Vec<[P3; 3]>,
    /// Surface normals at each triangle's corners (empty when unknown).
    pub normals: Vec<[P3; 3]>,
    /// Exact topological edges as polylines.
    pub edges: Vec<Vec<P3>>,
}

impl SolidData {
    pub fn bounds(&self) -> (P3, P3) {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for t in &self.triangles {
            for p in t {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
        (lo, hi)
    }
    pub fn size(&self) -> f64 {
        let (lo, hi) = self.bounds();
        len3(sub3(hi, lo)).max(1e-9)
    }
}

/// Möller–Trumbore: the ray parameter where `origin + t·dir` meets the
/// triangle, if it does.
fn ray_hit(origin: P3, dir: P3, tri: &[P3; 3]) -> Option<f64> {
    let e1 = sub3(tri[1], tri[0]);
    let e2 = sub3(tri[2], tri[0]);
    let p = cross(dir, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-14 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub3(origin, tri[0]);
    let u = dot(s, p) * inv;
    if !(-1e-12..=1.0 + 1e-12).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(dir, q) * inv;
    if v < -1e-12 || u + v > 1.0 + 1e-12 {
        return None;
    }
    Some(dot(e2, q) * inv)
}

/// Barycentric coordinates of `p` in `tri` when `p` lies on it (within
/// `tol` of its plane and inside its edges).
fn on_triangle(p: P3, tri: &[P3; 3], tol: f64) -> Option<[f64; 3]> {
    let e1 = sub3(tri[1], tri[0]);
    let e2 = sub3(tri[2], tri[0]);
    let n = cross(e1, e2);
    let nl = len3(n);
    if nl < 1e-18 {
        return None;
    }
    let w = sub3(p, tri[0]);
    if (dot(w, n) / nl).abs() > tol {
        return None;
    }
    let (d00, d01, d11) = (dot(e1, e1), dot(e1, e2), dot(e2, e2));
    let (d20, d21) = (dot(w, e1), dot(w, e2));
    let den = d00 * d11 - d01 * d01;
    let v = (d11 * d20 - d01 * d21) / den;
    let u = (d00 * d21 - d01 * d20) / den;
    let slack = tol / nl.sqrt().max(1e-12);
    (v >= -slack && u >= -slack && v + u <= 1.0 + slack).then_some([1.0 - v - u, v, u])
}

/// A tangent edge (between faces that meet smoothly, like the seams of a
/// split cylinder) is not drawn: the surface normals on both sides of its
/// midpoint agree.
fn is_tangent_edge(edge: &[P3], solid: &SolidData, tol: f64) -> bool {
    if solid.normals.len() != solid.triangles.len() || edge.len() < 2 {
        return false;
    }
    let mid = lerp3(
        edge[edge.len() / 2 - usize::from(edge.len() % 2 == 0)],
        edge[edge.len() / 2],
        0.5,
    );
    let mut normals: Vec<P3> = Vec::new();
    for (t, n) in solid.triangles.iter().zip(&solid.normals) {
        if let Some(b) = on_triangle(mid, t, tol) {
            let v = [
                n[0][0] * b[0] + n[1][0] * b[1] + n[2][0] * b[2],
                n[0][1] * b[0] + n[1][1] * b[1] + n[2][1] * b[2],
                n[0][2] * b[0] + n[1][2] * b[1] + n[2][2] * b[2],
            ];
            let l = len3(v);
            if l > 1e-12 {
                normals.push([v[0] / l, v[1] / l, v[2] / l]);
            }
        }
    }
    normals.len() >= 2
        && normals
            .iter()
            .all(|a| normals.iter().all(|b| dot(*a, *b) > 0.9995))
}

/// Whether `p` (on the solid's surface) is covered, seen along `view`.
fn hidden(p: P3, view: P3, solid: &SolidData, eps: f64) -> bool {
    let toward = [-view[0], -view[1], -view[2]];
    solid
        .triangles
        .iter()
        .any(|t| ray_hit(p, toward, t).is_some_and(|d| d > eps))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shape2 {
    Line { a: P2, b: P2 },
    Polyline { pts: Vec<P2> },
    Arc { c: P2, r: f64, start: f64, end: f64 },
    Circle { c: P2, r: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub kind: ViewKind,
    pub visible: Vec<Shape2>,
    pub hidden: Vec<Shape2>,
    pub lo: P2,
    pub hi: P2,
}

impl View {
    pub fn width(&self) -> f64 {
        self.hi[0] - self.lo[0]
    }
    pub fn height(&self) -> f64 {
        self.hi[1] - self.lo[1]
    }
    /// Visible circles (with arcs merged), for centre marks and dimensions.
    pub fn circles(&self) -> Vec<(P2, f64)> {
        self.visible
            .iter()
            .filter_map(|s| match s {
                Shape2::Circle { c, r } => Some((*c, *r)),
                _ => None,
            })
            .collect()
    }
}

fn point_seg_dist(p: P2, a: P2, b: P2) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let l2 = d[0] * d[0] + d[1] * d[1];
    if l2 < 1e-24 {
        return d2(p, a);
    }
    let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / l2).clamp(0.0, 1.0);
    d2(p, [a[0] + d[0] * t, a[1] + d[1] * t])
}

/// Circle through three points.
fn circle3(a: P2, b: P2, c: P2) -> Option<(P2, f64)> {
    let d = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
    if d.abs() < 1e-14 {
        return None;
    }
    let sq = |p: P2| p[0] * p[0] + p[1] * p[1];
    let ux = (sq(a) * (b[1] - c[1]) + sq(b) * (c[1] - a[1]) + sq(c) * (a[1] - b[1])) / d;
    let uy = (sq(a) * (c[0] - b[0]) + sq(b) * (a[0] - c[0]) + sq(c) * (b[0] - a[0])) / d;
    let center = [ux, uy];
    Some((center, d2(center, a)))
}

/// A run of points as a line, a circular arc, or a polyline.
fn classify(pts: &[P2], tol: f64) -> Option<Shape2> {
    let mut clean: Vec<P2> = Vec::with_capacity(pts.len());
    for p in pts {
        if clean.last().is_none_or(|q| d2(*q, *p) > tol * 0.01) {
            clean.push(*p);
        }
    }
    if clean.len() < 2 {
        return None;
    }
    let (a, b) = (clean[0], *clean.last().unwrap());
    let span = clean.windows(2).map(|w| d2(w[0], w[1])).sum::<f64>();
    if span < tol {
        return None;
    }
    if clean.iter().all(|p| point_seg_dist(*p, a, b) <= tol) && d2(a, b) > tol {
        return Some(Shape2::Line { a, b });
    }
    if clean.len() >= 4 {
        if let Some((c, r)) =
            circle3(a, clean[clean.len() / 2], b).or_else(|| circle3(clean[0], clean[1], clean[2]))
        {
            if r < span * 50.0
                && clean
                    .iter()
                    .all(|p| (d2(*p, c) - r).abs() <= tol.max(r * 1e-3))
            {
                // Signed sweep along the points.
                let ang = |p: P2| (p[1] - c[1]).atan2(p[0] - c[0]);
                let mut sweep = 0.0;
                for w in clean.windows(2) {
                    let mut d = ang(w[1]) - ang(w[0]);
                    while d > std::f64::consts::PI {
                        d -= std::f64::consts::TAU;
                    }
                    while d < -std::f64::consts::PI {
                        d += std::f64::consts::TAU;
                    }
                    sweep += d;
                }
                let (s, e) = if sweep >= 0.0 {
                    (ang(a), ang(a) + sweep)
                } else {
                    (ang(a) + sweep, ang(a))
                };
                return Some(Shape2::Arc {
                    c,
                    r,
                    start: s.to_degrees(),
                    end: e.to_degrees(),
                });
            }
        }
    }
    Some(Shape2::Polyline { pts: clean })
}

/// Join arcs of one circle into a full circle when they close it.
fn merge_arcs(shapes: Vec<Shape2>, tol: f64) -> Vec<Shape2> {
    let mut out = Vec::new();
    let mut arcs: Vec<(P2, f64, f64)> = Vec::new(); // centre, radius, total sweep
    let mut groups: Vec<Vec<Shape2>> = Vec::new();
    for s in shapes {
        if let Shape2::Arc { c, r, start, end } = &s {
            if let Some(k) = arcs
                .iter()
                .position(|(c2, r2, _)| d2(*c, *c2) <= tol && (r - r2).abs() <= tol)
            {
                arcs[k].2 += end - start;
                groups[k].push(s);
            } else {
                arcs.push((*c, *r, end - start));
                groups.push(vec![s]);
            }
        } else {
            out.push(s);
        }
    }
    for ((c, r, sweep), g) in arcs.into_iter().zip(groups) {
        if sweep >= 359.0 {
            out.push(Shape2::Circle { c, r });
        } else {
            out.extend(g);
        }
    }
    out
}

/// Project the solid into one view. `silhouettes` are the kernel's smooth-face
/// contour segments for this view direction.
pub fn project(
    kind: ViewKind,
    solid: &SolidData,
    silhouettes: &[[P3; 2]],
    with_hidden: bool,
) -> View {
    let (view, right, up) = kind.frame();
    let size = solid.size();
    let eps = size * 1e-6;
    let tol = size * 2e-4;
    let piece = size / 120.0;
    let to2 = |p: P3| [dot(p, right), dot(p, up)];

    // Every edge and silhouette as polylines of short, classified pieces.
    let mut polylines: Vec<Vec<P3>> = solid
        .edges
        .iter()
        .filter(|e| !is_tangent_edge(e, solid, tol))
        .cloned()
        .collect();
    polylines.extend(silhouettes.iter().map(|s| vec![s[0], s[1]]));
    let mut vis_runs: Vec<Vec<P2>> = Vec::new();
    let mut hid_runs: Vec<Vec<P2>> = Vec::new();
    for poly in &polylines {
        let mut current: Option<(bool, Vec<P2>)> = None;
        let mut flush = |cur: &mut Option<(bool, Vec<P2>)>| {
            if let Some((h, run)) = cur.take() {
                if h {
                    hid_runs.push(run)
                } else {
                    vis_runs.push(run)
                }
            }
        };
        for w in poly.windows(2) {
            let n = ((len3(sub3(w[1], w[0])) / piece).ceil() as usize).clamp(1, 400);
            for k in 0..n {
                let (p0, p1) = (
                    lerp3(w[0], w[1], k as f64 / n as f64),
                    lerp3(w[0], w[1], (k + 1) as f64 / n as f64),
                );
                let h = hidden(lerp3(p0, p1, 0.5), view, solid, eps);
                match &mut current {
                    Some((ch, run)) if *ch == h => run.push(to2(p1)),
                    _ => {
                        flush(&mut current);
                        current = Some((h, vec![to2(p0), to2(p1)]));
                    }
                }
            }
        }
        flush(&mut current);
    }

    let classify_all = |runs: &[Vec<P2>]| -> Vec<Shape2> {
        merge_arcs(
            runs.iter().filter_map(|r| classify(r, tol)).collect(),
            tol * 5.0,
        )
    };
    let visible = classify_all(&vis_runs);
    // Visible lines win: drop hidden pieces that lie on a visible line.
    let vis_segments: Vec<(P2, P2)> = vis_runs
        .iter()
        .flat_map(|r| r.windows(2).map(|w| (w[0], w[1])).collect::<Vec<_>>())
        .collect();
    let on_visible = |p: P2| {
        vis_segments
            .iter()
            .any(|(a, b)| point_seg_dist(p, *a, *b) <= tol * 2.0)
    };
    let hidden_runs: Vec<Vec<P2>> = if with_hidden && kind != ViewKind::Iso {
        hid_runs
            .into_iter()
            .filter(|r| {
                let mids: Vec<P2> = r
                    .windows(2)
                    .map(|w| [(w[0][0] + w[1][0]) / 2.0, (w[0][1] + w[1][1]) / 2.0])
                    .collect();
                !mids.iter().all(|m| on_visible(*m))
            })
            .collect()
    } else {
        Vec::new()
    };
    // Merge visible circles drawn as several arcs is done above; dedupe
    // coincident visible shapes (front and back silhouettes can meet).
    let mut vis_unique: Vec<Shape2> = Vec::new();
    for s in visible {
        if !vis_unique.iter().any(|t| same_shape(t, &s, tol * 2.0)) {
            vis_unique.push(s);
        }
    }
    let mut hidden: Vec<Shape2> = Vec::new();
    for s in classify_all(&hidden_runs) {
        if !vis_unique
            .iter()
            .chain(hidden.iter())
            .any(|t| same_shape(t, &s, tol * 2.0))
        {
            hidden.push(s);
        }
    }

    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for t in &solid.triangles {
        for p in t {
            let q = to2(*p);
            lo = [lo[0].min(q[0]), lo[1].min(q[1])];
            hi = [hi[0].max(q[0]), hi[1].max(q[1])];
        }
    }
    View {
        kind,
        visible: vis_unique,
        hidden,
        lo,
        hi,
    }
}

fn same_shape(a: &Shape2, b: &Shape2, tol: f64) -> bool {
    match (a, b) {
        (Shape2::Line { a: a0, b: a1 }, Shape2::Line { a: b0, b: b1 }) => {
            (d2(*a0, *b0) <= tol && d2(*a1, *b1) <= tol)
                || (d2(*a0, *b1) <= tol && d2(*a1, *b0) <= tol)
        }
        (Shape2::Circle { c, r }, Shape2::Circle { c: c2, r: r2 }) => {
            d2(*c, *c2) <= tol && (r - r2).abs() <= tol
        }
        _ => false,
    }
}

// ── Layout and output ───────────────────────────────────────────────────────

/// Where each view's lower-left corner goes: third-angle arrangement (top
/// above front, right beside front, iso to the upper right) with `gap`
/// between views, starting at `origin`.
pub fn layout(views: &[View], origin: P2, gap: f64) -> Vec<P2> {
    let get = |k: ViewKind| views.iter().find(|v| v.kind == k);
    let front_w = get(ViewKind::Front).map_or(0.0, View::width);
    let front_h = get(ViewKind::Front).map_or(0.0, View::height);
    let right_w = get(ViewKind::Right).map_or(0.0, View::width);
    views
        .iter()
        .map(|v| match v.kind {
            ViewKind::Front => origin,
            ViewKind::Top => [origin[0], origin[1] + front_h + gap],
            ViewKind::Right => [origin[0] + front_w + gap, origin[1]],
            ViewKind::Iso => [
                origin[0] + front_w + gap + if right_w > 0.0 { right_w + gap } else { 0.0 },
                origin[1] + front_h + gap,
            ],
        })
        .collect()
}

fn place(p: P2, v: &View, at: P2) -> P2 {
    [p[0] - v.lo[0] + at[0], p[1] - v.lo[1] + at[1]]
}

fn shape_json(s: &Shape2, v: &View, at: P2, layer: &str) -> Value {
    match s {
        Shape2::Line { a, b } => {
            json!({"type":"Line","start":place(*a, v, at),"end":place(*b, v, at),"layer":layer})
        }
        Shape2::Polyline { pts } => {
            json!({"type":"LwPolyline","vertices":pts.iter().map(|p| place(*p, v, at)).collect::<Vec<_>>(),"closed":false,"layer":layer})
        }
        Shape2::Arc { c, r, start, end } => {
            json!({"type":"Arc","center":place(*c, v, at),"radius":r,"start_angle_deg":start,"end_angle_deg":end,"layer":layer})
        }
        Shape2::Circle { c, r } => {
            json!({"type":"Circle","center":place(*c, v, at),"radius":r,"layer":layer})
        }
    }
}

fn circle_name(kind: ViewKind, n: usize) -> String {
    format!("circle_{}_{n}", kind.label().to_ascii_lowercase())
}

/// `entities_create` steps for the views, their centre marks and labels.
pub fn views_steps(views: &[View], offsets: &[P2], text_height: f64) -> Vec<Value> {
    let mut steps = Vec::new();
    for (v, at) in views.iter().zip(offsets) {
        // Circles get their own step so dimensions can pick them by name.
        let mut e: Vec<Value> = v
            .visible
            .iter()
            .filter(|s| !matches!(s, Shape2::Circle { .. }))
            .map(|s| shape_json(s, v, *at, L_VISIBLE))
            .collect();
        e.extend(v.hidden.iter().map(|s| {
            let mut j = shape_json(s, v, *at, L_HIDDEN);
            j["color"] = json!(8);
            j
        }));
        for (n, (c, r)) in v.circles().into_iter().enumerate() {
            steps.push(json!({"op":"entities_create","entities":[shape_json(&Shape2::Circle { c, r }, v, *at, L_VISIBLE)],"bind":circle_name(v.kind, n)}));
        }
        if v.kind != ViewKind::Iso {
            for (c, r) in v.circles() {
                let p = place(c, v, *at);
                let ext = r + text_height * 0.8;
                e.push(json!({"type":"Line","start":[p[0] - ext, p[1]],"end":[p[0] + ext, p[1]],"layer":L_CENTER}));
                e.push(json!({"type":"Line","start":[p[0], p[1] - ext],"end":[p[0], p[1] + ext],"layer":L_CENTER}));
            }
        }
        let label_at = [
            at[0],
            at[1] - text_height * if v.kind == ViewKind::Iso { 2.0 } else { 5.5 },
        ];
        e.push(json!({"type":"Text","value":v.kind.label(),"position":label_at,"height":text_height,"layer":L_LABEL}));
        steps.push(json!({"op":"entities_create","entities":e,"note":format!("{} view", v.kind.label().to_lowercase())}));
    }
    steps
}

/// A dimension to place, in a view's own 2D coordinates.
#[derive(Debug, Clone, PartialEq)]
pub enum Dim {
    /// Horizontal (`vertical == false`) or vertical distance between two
    /// points, with the dimension line offset to the outside of the view.
    Linear {
        view: ViewKind,
        a: P2,
        b: P2,
        vertical: bool,
        offset: f64,
    },
    /// The diameter of circle `index` of the view (see [`View::circles`]).
    Diameter {
        view: ViewKind,
        index: usize,
        c: P2,
        r: f64,
    },
}

/// A first-pass scheme a drafter would start from: overall width and height
/// on the front view, depth on the top (or right) view, every distinct hole
/// size once, and each hole centre located from the view's lower-left
/// corner (in the view where the holes show as circles).
pub fn basic_dims(views: &[View], spacing: f64) -> Vec<Dim> {
    let get = |k: ViewKind| views.iter().find(|v| v.kind == k);
    let mut dims = Vec::new();
    let overall = |v: &View, horizontal: bool, vertical: bool, dims: &mut Vec<Dim>| {
        if horizontal {
            dims.push(Dim::Linear {
                view: v.kind,
                a: v.lo,
                b: [v.hi[0], v.lo[1]],
                vertical: false,
                offset: -spacing * 2.0,
            });
        }
        if vertical {
            dims.push(Dim::Linear {
                view: v.kind,
                a: v.lo,
                b: [v.lo[0], v.hi[1]],
                vertical: true,
                offset: -spacing * 2.0,
            });
        }
    };
    match (
        get(ViewKind::Front),
        get(ViewKind::Top),
        get(ViewKind::Right),
    ) {
        (Some(f), top, right) => {
            overall(f, true, true, &mut dims);
            if let Some(t) = top {
                overall(t, false, true, &mut dims);
            } else if let Some(r) = right {
                overall(r, true, false, &mut dims);
            }
        }
        (None, Some(t), _) => overall(t, true, true, &mut dims),
        (None, None, Some(r)) => overall(r, true, true, &mut dims),
        _ => {}
    }
    // Holes: the orthographic view with the most circles.
    if let Some(v) = views
        .iter()
        .filter(|v| v.kind != ViewKind::Iso)
        .max_by_key(|v| v.circles().len())
        .filter(|v| !v.circles().is_empty())
    {
        let mut sizes: Vec<f64> = Vec::new();
        let mut xs: Vec<f64> = Vec::new();
        let mut ys: Vec<f64> = Vec::new();
        for (index, (c, r)) in v.circles().into_iter().enumerate().take(12) {
            if !sizes.iter().any(|s| (s - r).abs() <= r * 1e-3) {
                sizes.push(r);
                dims.push(Dim::Diameter {
                    view: v.kind,
                    index,
                    c,
                    r,
                });
            }
            // Each distinct centre coordinate is located once.
            let tol = (v.width() + v.height()) * 1e-4;
            if !xs.iter().any(|x| (x - c[0]).abs() <= tol) && (c[0] - v.lo[0]).abs() > tol {
                xs.push(c[0]);
                let k = xs.len() as f64;
                dims.push(Dim::Linear {
                    view: v.kind,
                    a: [v.lo[0], v.hi[1]],
                    b: [c[0], c[1]],
                    vertical: false,
                    offset: spacing * (1.0 + k),
                });
            }
            if !ys.iter().any(|y| (y - c[1]).abs() <= tol) && (c[1] - v.lo[1]).abs() > tol {
                ys.push(c[1]);
                let k = ys.len() as f64;
                dims.push(Dim::Linear {
                    view: v.kind,
                    a: [v.hi[0], v.lo[1]],
                    b: [c[0], c[1]],
                    vertical: true,
                    offset: spacing * (1.0 + k),
                });
            }
        }
    }
    dims
}

fn fmt(x: f64) -> String {
    let s = format!("{x:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Steps that draw `dims` with DIMLINEAR / DIMDIAMETER on layer
/// [`L_DIMS`]. Diameters pick the circles [`views_steps`] created, by the
/// names it bound them to, so they stay associated with them.
pub fn dims_steps(dims: &[Dim], views: &[View], offsets: &[P2], restore_layer: &str) -> Vec<Value> {
    if dims.is_empty() {
        return Vec::new();
    }
    let at = |k: ViewKind| {
        views
            .iter()
            .zip(offsets)
            .find(|(v, _)| v.kind == k)
            .map(|(v, o)| (v, *o))
    };
    let mut steps = vec![
        json!({"op":"entities_create","entities":[{"type":"Point","location":[offsets[0][0], offsets[0][1], 0.0],"layer":L_DIMS}],"bind":"dim_anchor","note":"dimensions"}),
        json!({"op":"sysvar","set":{"clayer":L_DIMS}}),
        json!({"op":"entities_delete","handles":"$dim_anchor"}),
    ];
    for d in dims {
        match d {
            Dim::Linear {
                view,
                a,
                b,
                vertical,
                offset,
            } => {
                let Some((v, o)) = at(*view) else { continue };
                let (pa, pb) = (place(*a, v, o), place(*b, v, o));
                // The dimension line: below (or left of) the view for a
                // negative offset, above (or right of) it for a positive one.
                let loc = if *vertical {
                    let x = if *offset < 0.0 {
                        o[0] + offset
                    } else {
                        o[0] + v.width() + offset
                    };
                    [x, (pa[1] + pb[1]) / 2.0]
                } else {
                    let y = if *offset < 0.0 {
                        o[1] + offset
                    } else {
                        o[1] + v.height() + offset
                    };
                    [(pa[0] + pb[0]) / 2.0, y]
                };
                if (if *vertical {
                    (pa[1] - pb[1]).abs()
                } else {
                    (pa[0] - pb[0]).abs()
                }) < 1e-9
                {
                    continue;
                }
                steps.push(json!({"op":"start","cmd":"DIMLINEAR"}));
                steps.push(json!({"op":"input","kind":"point","point":[pa[0], pa[1], 0.0]}));
                steps.push(json!({"op":"input","kind":"point","point":[pb[0], pb[1], 0.0]}));
                steps.push(json!({"op":"input","kind":"point","point":[loc[0], loc[1], 0.0],"note":format!("{} {} dimension", view.label().to_lowercase(), if *vertical { "vertical" } else { "horizontal" })}));
            }
            Dim::Diameter { view, index, c, r } => {
                let Some((v, o)) = at(*view) else { continue };
                let p = place(*c, v, o);
                steps.push(json!({"op":"start","cmd":"DIMDIAMETER"}));
                steps.push(json!({"op":"input","kind":"entity","handle":format!("${}", circle_name(*view, *index)),"point":[p[0] + r, p[1], 0.0]}));
                steps.push(json!({"op":"input","kind":"point","point":[p[0] + r * 1.8, p[1] + r * 1.8, 0.0],"note":format!("Ø{}", fmt(2.0 * r))}));
            }
        }
    }
    steps.push(json!({"op":"sysvar","set":{"clayer":restore_layer}}));
    steps
}

// ── AI dimension scheme ─────────────────────────────────────────────────────

/// Candidate points per orthographic view (corners of straight edges and
/// circle centres), numbered for the model.
pub fn feature_points(v: &View) -> Vec<(P2, &'static str)> {
    let mut pts: Vec<(P2, &'static str)> = Vec::new();
    let tol = (v.width() + v.height()) * 1e-4;
    let add = |p: P2, what: &'static str, pts: &mut Vec<(P2, &'static str)>| {
        if !pts.iter().any(|(q, _)| d2(*q, p) <= tol) {
            pts.push((p, what));
        }
    };
    for s in &v.visible {
        match s {
            Shape2::Line { a, b } => {
                add(*a, "corner", &mut pts);
                add(*b, "corner", &mut pts);
            }
            Shape2::Circle { c, .. } | Shape2::Arc { c, .. } => add(*c, "centre", &mut pts),
            Shape2::Polyline { pts: p } => {
                if let (Some(a), Some(b)) = (p.first(), p.last()) {
                    add(*a, "end", &mut pts);
                    add(*b, "end", &mut pts);
                }
            }
        }
    }
    pts.truncate(150);
    pts
}

pub fn dims_prompt(views: &[View], standard: &str) -> String {
    let mut listing = String::new();
    for v in views.iter().filter(|v| v.kind != ViewKind::Iso) {
        listing.push_str(&format!(
            "\n{} view ({} × {}):\n",
            v.kind.label(),
            fmt(v.width()),
            fmt(v.height())
        ));
        for (n, (p, what)) in feature_points(v).iter().enumerate() {
            listing.push_str(&format!(
                "  p{n} {what} ({}, {})\n",
                fmt(p[0] - v.lo[0]),
                fmt(p[1] - v.lo[1])
            ));
        }
        for (n, (c, r)) in v.circles().iter().enumerate() {
            listing.push_str(&format!(
                "  c{n} circle centre ({}, {}) Ø{}\n",
                fmt(c[0] - v.lo[0]),
                fmt(c[1] - v.lo[1]),
                fmt(2.0 * r)
            ));
        }
    }
    format!(
        r#"You are a mechanical drafter dimensioning a part to {standard}. The part's orthographic views list their feature points (coordinates from each view's lower-left corner) and circles:
{listing}
Choose a complete, non-redundant dimension scheme: overall sizes, every feature size and location exactly once, holes located from a common datum, diameters for holes, no closed dimension chains, each dimension in the view where the feature shows its true shape. Do not give values — they are measured.

Answer with ONE JSON object:
{{"dimensions": [
  {{"view": "front" | "top" | "right", "kind": "horizontal" | "vertical", "a": "p3", "b": "p7"}},
  {{"view": "top", "kind": "diameter", "circle": "c0"}}
]}}"#
    )
}

/// Parse the model's scheme into dimensions; unknown references are dropped.
pub fn parse_dims(reply: &str, views: &[View], spacing: f64) -> Result<Vec<Dim>, String> {
    let v = super::client::extract_json(reply).ok_or("the model did not answer with JSON")?;
    let list = v["dimensions"]
        .as_array()
        .ok_or("the reply has no \"dimensions\" list")?;
    let mut out = Vec::new();
    let mut stack = std::collections::HashMap::<(ViewKind, bool, bool), usize>::new();
    for d in list.iter().take(80) {
        let kind = match d["view"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "front" => ViewKind::Front,
            "top" | "plan" => ViewKind::Top,
            "right" | "side" => ViewKind::Right,
            _ => continue,
        };
        let Some(view) = views.iter().find(|x| x.kind == kind) else {
            continue;
        };
        let pts = feature_points(view);
        let circles = view.circles();
        let point = |s: &Value| -> Option<P2> {
            let s = s.as_str()?;
            if let Some(n) = s.strip_prefix('p') {
                pts.get(n.parse::<usize>().ok()?).map(|(p, _)| *p)
            } else if let Some(n) = s.strip_prefix('c') {
                circles.get(n.parse::<usize>().ok()?).map(|(c, _)| *c)
            } else {
                None
            }
        };
        match d["kind"].as_str().unwrap_or("") {
            "diameter" | "radius" => {
                if let Some(index) = d["circle"]
                    .as_str()
                    .and_then(|s| s.strip_prefix('c'))
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    if let Some((c, r)) = circles.get(index) {
                        out.push(Dim::Diameter {
                            view: kind,
                            index,
                            c: *c,
                            r: *r,
                        });
                    }
                }
            }
            k @ ("horizontal" | "vertical") => {
                let (Some(a), Some(b)) = (point(&d["a"]), point(&d["b"])) else {
                    continue;
                };
                let vertical = k == "vertical";
                if (if vertical { a[1] - b[1] } else { a[0] - b[0] }).abs() < 1e-9 {
                    continue;
                }
                // Put each dimension outside the view on the nearer side,
                // stacking further out for each one already there.
                let mid = if vertical {
                    (a[0] + b[0]) / 2.0
                } else {
                    (a[1] + b[1]) / 2.0
                };
                let centre = if vertical {
                    (view.lo[0] + view.hi[0]) / 2.0
                } else {
                    (view.lo[1] + view.hi[1]) / 2.0
                };
                let outside_positive = mid > centre;
                let slot = stack.entry((kind, vertical, outside_positive)).or_insert(0);
                *slot += 1;
                let offset =
                    spacing * (1.0 + *slot as f64) * if outside_positive { 1.0 } else { -1.0 };
                out.push(Dim::Linear {
                    view: kind,
                    a,
                    b,
                    vertical,
                    offset,
                });
            }
            _ => {}
        }
    }
    if out.is_empty() {
        return Err("the scheme had no usable dimensions".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 40 × 20 × 10 box (centred on the origin in X and Y, z 0 → 10) as
    /// triangles and its 12 edges.
    fn boxed() -> SolidData {
        let (x0, x1, y0, y1, z0, z1) = (-20.0, 20.0, -10.0, 10.0, 0.0, 10.0);
        let c = [
            [x0, y0, z0],
            [x1, y0, z0],
            [x1, y1, z0],
            [x0, y1, z0],
            [x0, y0, z1],
            [x1, y0, z1],
            [x1, y1, z1],
            [x0, y1, z1],
        ];
        let quads = [
            [0, 1, 2, 3],
            [4, 5, 6, 7],
            [0, 1, 5, 4],
            [1, 2, 6, 5],
            [2, 3, 7, 6],
            [3, 0, 4, 7],
        ];
        let mut triangles = Vec::new();
        for q in quads {
            triangles.push([c[q[0]], c[q[1]], c[q[2]]]);
            triangles.push([c[q[0]], c[q[2]], c[q[3]]]);
        }
        let pairs = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        SolidData {
            triangles,
            normals: Vec::new(),
            edges: pairs.iter().map(|(a, b)| vec![c[*a], c[*b]]).collect(),
        }
    }

    #[test]
    fn a_box_front_view_is_a_rectangle_with_no_hidden_lines() {
        let v = project(ViewKind::Front, &boxed(), &[], true);
        let lines = v
            .visible
            .iter()
            .filter(|s| matches!(s, Shape2::Line { .. }))
            .count();
        assert_eq!(lines, 4, "{:?}", v.visible);
        assert!(
            v.hidden.is_empty(),
            "back edges sit under visible ones: {:?}",
            v.hidden
        );
        assert_eq!((v.width(), v.height()), (40.0, 10.0));
        let t = project(ViewKind::Top, &boxed(), &[], true);
        assert_eq!((t.width(), t.height()), (40.0, 20.0));
        let r = project(ViewKind::Right, &boxed(), &[], true);
        assert_eq!((r.width(), r.height()), (20.0, 10.0));
    }

    #[test]
    fn iso_shows_three_faces_and_nothing_hidden() {
        let v = project(ViewKind::Iso, &boxed(), &[], true);
        // 9 of the box's 12 edges are visible from a corner.
        assert_eq!(v.visible.len(), 9, "{:?}", v.visible);
        assert!(v.hidden.is_empty());
    }

    #[test]
    fn an_edge_behind_the_solid_is_hidden() {
        // A rod floating behind the box (seen from the front) is hidden.
        let mut s = boxed();
        s.edges.push(vec![[-5.0, 15.0, 5.0], [5.0, 15.0, 5.0]]);
        let v = project(ViewKind::Front, &s, &[], true);
        assert_eq!(v.hidden.len(), 1, "{:?}", v.hidden);
        let Shape2::Line { a, b } = v.hidden[0] else {
            panic!()
        };
        assert!((a[1] - 5.0).abs() < 1e-9 && (b[0] - a[0]).abs() > 9.99);
        assert!(project(ViewKind::Front, &s, &[], false).hidden.is_empty());
    }

    #[test]
    fn circular_runs_become_circles_and_arcs() {
        let ring: Vec<P2> = (0..=16)
            .map(|k| {
                let t = std::f64::consts::FRAC_PI_2 * k as f64 / 16.0;
                [5.0 + 4.0 * t.cos(), 4.0 * t.sin()]
            })
            .collect();
        let Some(Shape2::Arc { c, r, start, end }) = classify(&ring, 1e-3) else {
            panic!()
        };
        assert!((c[0] - 5.0).abs() < 1e-9 && (r - 4.0).abs() < 1e-9);
        assert!((end - start - 90.0).abs() < 1e-6);
        let quarter = |k: f64| Shape2::Arc {
            c: [5.0, 0.0],
            r: 4.0,
            start: 90.0 * k,
            end: 90.0 * (k + 1.0),
        };
        let merged = merge_arcs(
            vec![quarter(0.0), quarter(1.0), quarter(2.0), quarter(3.0)],
            1e-3,
        );
        assert_eq!(
            merged,
            vec![Shape2::Circle {
                c: [5.0, 0.0],
                r: 4.0
            }]
        );
    }

    #[test]
    fn layout_and_basic_dimensions() {
        let s = boxed();
        let views: Vec<View> = [ViewKind::Front, ViewKind::Top, ViewKind::Right]
            .iter()
            .map(|k| project(*k, &s, &[], true))
            .collect();
        let o = layout(&views, [100.0, 0.0], 15.0);
        assert_eq!(o, vec![[100.0, 0.0], [100.0, 25.0], [155.0, 0.0]]);
        let dims = basic_dims(&views, 4.0);
        assert_eq!(dims.len(), 3, "width, height, depth: {dims:?}");
        let steps = dims_steps(&dims, &views, &o, "0");
        assert_eq!(steps.iter().filter(|s| s["cmd"] == "DIMLINEAR").count(), 3);
        assert_eq!(steps.last().unwrap()["set"]["clayer"], "0");
    }

    #[test]
    fn parses_an_ai_scheme() {
        let s = boxed();
        let views = vec![project(ViewKind::Front, &s, &[], true)];
        let prompt = dims_prompt(&views, "ISO 129");
        assert!(prompt.contains("FRONT view (40 × 10)"));
        let d = parse_dims(r#"{"dimensions":[{"view":"front","kind":"horizontal","a":"p0","b":"p1"},{"view":"top","kind":"vertical","a":"p0","b":"p1"},{"view":"front","kind":"diameter","circle":"c9"}]}"#, &views, 4.0).unwrap();
        assert_eq!(d.len(), 1, "unknown view / circle dropped: {d:?}");
    }
}
