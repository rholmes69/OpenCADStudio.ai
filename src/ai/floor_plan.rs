//! Floor-plan assistant (AIPLAN): a small architectural model — walls with
//! door and window openings, and named rooms — that comes from a plan image
//! (vision model), a written brief (language model) or selected lines, and
//! is drawn as a layered 2D plan with a room schedule and, optionally, a 3D
//! massing model.
//!
//! Layers follow the AIA CAD layer guidelines: `A-WALL` (solid poché),
//! `A-DOOR`, `A-GLAZ`, `A-AREA` (room boundaries), `A-AREA-IDEN` (room
//! labels), `A-ANNO-SCHD` (the schedule) and `A-WALL-3D` (massing solids).

use serde_json::{json, Value};

pub const L_WALL: &str = "A-WALL";
pub const L_DOOR: &str = "A-DOOR";
pub const L_GLAZ: &str = "A-GLAZ";
pub const L_AREA: &str = "A-AREA";
pub const L_IDEN: &str = "A-AREA-IDEN";
pub const L_SCHD: &str = "A-ANNO-SCHD";
pub const L_3D: &str = "A-WALL-3D";

pub const MAX_WALLS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningKind {
    Door,
    Window,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Wall {
    /// Centre line.
    pub a: [f64; 2],
    pub b: [f64; 2],
    pub thickness: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Opening {
    pub kind: OpeningKind,
    pub wall: usize,
    /// Distance from the wall's start to the opening's centre.
    pub at: f64,
    pub width: f64,
    /// Door: leaf height. Window: glass height.
    pub height: f64,
    /// Window sill above the floor (0 for doors).
    pub sill: f64,
    /// Door hinge at the wall-start side of the opening.
    pub hinge_start: bool,
    /// Door swings to the left of the wall direction (a → b).
    pub swing_left: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Room {
    pub name: String,
    pub points: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub units: String,
    pub wall_height: f64,
    pub walls: Vec<Wall>,
    pub openings: Vec<Opening>,
    pub rooms: Vec<Room>,
    pub notes: String,
}

// ── Geometry helpers ────────────────────────────────────────────────────────

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn add(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}
fn mul(a: [f64; 2], k: f64) -> [f64; 2] {
    [a[0] * k, a[1] * k]
}
fn len(a: [f64; 2]) -> f64 {
    a[0].hypot(a[1])
}

impl Wall {
    pub fn length(&self) -> f64 {
        len(sub(self.b, self.a))
    }
    /// Unit direction a → b and its left normal.
    pub fn frame(&self) -> ([f64; 2], [f64; 2]) {
        let d = sub(self.b, self.a);
        let l = len(d).max(1e-12);
        let u = [d[0] / l, d[1] / l];
        (u, [-u[1], u[0]])
    }
    /// The footprint between distances `s0` and `s1` along the wall
    /// (negative / beyond-length values reach past the ends).
    pub fn band(&self, s0: f64, s1: f64) -> Vec<[f64; 2]> {
        let (u, n) = self.frame();
        let h = self.thickness / 2.0;
        let p0 = add(self.a, mul(u, s0));
        let p1 = add(self.a, mul(u, s1));
        vec![
            add(p0, mul(n, -h)),
            add(p1, mul(n, -h)),
            add(p1, mul(n, h)),
            add(p0, mul(n, h)),
        ]
    }
}

/// Shoelace area (absolute) and perimeter of a closed polygon.
pub fn area_perimeter(points: &[[f64; 2]]) -> (f64, f64) {
    let n = points.len();
    if n < 3 {
        return (0.0, 0.0);
    }
    let mut a = 0.0;
    let mut p = 0.0;
    for i in 0..n {
        let (q, r) = (points[i], points[(i + 1) % n]);
        a += q[0] * r[1] - r[0] * q[1];
        p += len(sub(r, q));
    }
    (a.abs() / 2.0, p)
}

/// Area-weighted centroid, falling back to the vertex mean.
pub fn centroid(points: &[[f64; 2]]) -> [f64; 2] {
    let n = points.len();
    let mut a = 0.0;
    let (mut cx, mut cy) = (0.0, 0.0);
    for i in 0..n {
        let (q, r) = (points[i], points[(i + 1) % n]);
        let c = q[0] * r[1] - r[0] * q[1];
        a += c;
        cx += (q[0] + r[0]) * c;
        cy += (q[1] + r[1]) * c;
    }
    if a.abs() < 1e-12 {
        let m = points.iter().fold([0.0, 0.0], |s, p| add(s, *p));
        return mul(m, 1.0 / n.max(1) as f64);
    }
    [cx / (3.0 * a), cy / (3.0 * a)]
}

// ── Parsing and checks ──────────────────────────────────────────────────────

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|x: &f64| x.is_finite())
}
fn pt(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array()?;
    Some([num(a.first()?)?, num(a.get(1)?)?])
}

/// Parse a model reply (or stored JSON) into a plan and check it.
pub fn parse_plan(reply: &str) -> Result<Plan, String> {
    let v = super::client::extract_json(reply).ok_or("the answer is not a JSON floor plan")?;
    if let Some(m) = v["message"].as_str().filter(|m| !m.trim().is_empty()) {
        if v["walls"].as_array().is_none_or(|w| w.is_empty()) {
            return Err(m.trim().to_string());
        }
    }
    let default_t = num(&v["wall_thickness"]).filter(|t| *t > 0.0);
    let walls: Vec<Wall> = v["walls"]
        .as_array()
        .ok_or("the plan has no walls")?
        .iter()
        .enumerate()
        .map(|(n, w)| {
            let a = pt(&w["a"]).ok_or(format!("wall {n}: missing \"a\""))?;
            let b = pt(&w["b"]).ok_or(format!("wall {n}: missing \"b\""))?;
            let thickness = num(&w["t"])
                .or(default_t)
                .ok_or(format!("wall {n}: missing thickness \"t\""))?;
            Ok(Wall { a, b, thickness })
        })
        .collect::<Result<_, String>>()?;
    let openings = v["openings"]
        .as_array()
        .map(|list| {
            list.iter()
                .enumerate()
                .map(|(n, o)| {
                    let kind = match o["kind"]
                        .as_str()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .as_str()
                    {
                        "door" | "doorway" => OpeningKind::Door,
                        "window" => OpeningKind::Window,
                        other => return Err(format!("opening {n}: unknown kind \"{other}\"")),
                    };
                    let wall = o["wall"]
                        .as_u64()
                        .ok_or(format!("opening {n}: missing wall index"))?
                        as usize;
                    let width = num(&o["width"]).ok_or(format!("opening {n}: missing width"))?;
                    let swing = o["swing"].as_str().unwrap_or("").to_ascii_lowercase();
                    Ok(Opening {
                        kind,
                        wall,
                        at: num(&o["at"]).ok_or(format!("opening {n}: missing \"at\""))?,
                        width,
                        height: num(&o["height"]).unwrap_or(if kind == OpeningKind::Door {
                            2.1
                        } else {
                            1.2
                        }),
                        sill: if kind == OpeningKind::Door {
                            0.0
                        } else {
                            num(&o["sill"]).unwrap_or(0.9)
                        },
                        hinge_start: !swing.contains("end") && !swing.contains("right"),
                        swing_left: !swing.contains("out") && !swing.contains("rightside"),
                    })
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?
        .unwrap_or_default();
    let rooms = v["rooms"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| {
                    let mut points: Vec<[f64; 2]> =
                        r["points"].as_array()?.iter().filter_map(pt).collect();
                    if points.len() >= 2 && points.first() == points.last() {
                        points.pop();
                    }
                    (points.len() >= 3).then(|| Room {
                        name: r["name"].as_str().unwrap_or("Room").trim().to_string(),
                        points,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let plan = Plan {
        units: v["units"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase(),
        wall_height: num(&v["wall_height"]).filter(|h| *h > 0.0).unwrap_or(0.0),
        walls,
        openings,
        rooms,
        notes: v["notes"].as_str().unwrap_or("").trim().to_string(),
    };
    check(&plan)?;
    Ok(plan)
}

/// Structural checks: walls have length and thickness, openings fit their
/// wall and do not overlap each other.
pub fn check(plan: &Plan) -> Result<(), String> {
    if plan.walls.is_empty() {
        return Err("the plan has no walls".into());
    }
    if plan.walls.len() > MAX_WALLS {
        return Err(format!(
            "{} walls is more than the {MAX_WALLS} allowed",
            plan.walls.len()
        ));
    }
    for (n, w) in plan.walls.iter().enumerate() {
        if !(w.thickness > 0.0) {
            return Err(format!("wall {n} needs a positive thickness"));
        }
        if w.length() <= w.thickness * 0.5 {
            return Err(format!("wall {n} is too short for its thickness"));
        }
    }
    let mut per_wall: Vec<Vec<(f64, f64)>> = vec![Vec::new(); plan.walls.len()];
    for (n, o) in plan.openings.iter().enumerate() {
        let w = plan.walls.get(o.wall).ok_or(format!(
            "opening {n} refers to wall {} which does not exist",
            o.wall
        ))?;
        if !(o.width > 0.0) {
            return Err(format!("opening {n} needs a positive width"));
        }
        let (s0, s1) = (o.at - o.width / 2.0, o.at + o.width / 2.0);
        if s0 < -1e-9 || s1 > w.length() + 1e-9 {
            return Err(format!(
                "opening {n} ({:.2} wide at {:.2}) does not fit wall {} ({:.2} long)",
                o.width,
                o.at,
                o.wall,
                w.length()
            ));
        }
        if plan.wall_height > 0.0 && o.sill + o.height > plan.wall_height + 1e-9 {
            return Err(format!("opening {n} is taller than the wall"));
        }
        per_wall[o.wall].push((s0, s1));
    }
    for (w, spans) in per_wall.iter_mut().enumerate() {
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        if spans.windows(2).any(|p| p[1].0 < p[0].1 - 1e-9) {
            return Err(format!("two openings overlap on wall {w}"));
        }
    }
    Ok(())
}

/// Scale every length by `k` (unit conversion).
pub fn scale(plan: &mut Plan, k: f64) {
    if (k - 1.0).abs() < 1e-12 {
        return;
    }
    let s = |p: &mut [f64; 2]| {
        p[0] *= k;
        p[1] *= k;
    };
    plan.wall_height *= k;
    for w in &mut plan.walls {
        s(&mut w.a);
        s(&mut w.b);
        w.thickness *= k;
    }
    for o in &mut plan.openings {
        o.at *= k;
        o.width *= k;
        o.height *= k;
        o.sill *= k;
    }
    for r in &mut plan.rooms {
        r.points.iter_mut().for_each(s);
    }
}

pub fn extents(plan: &Plan) -> ([f64; 2], [f64; 2]) {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    let mut grow = |p: [f64; 2]| {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    };
    for w in &plan.walls {
        for p in w.band(-w.thickness / 2.0, w.length() + w.thickness / 2.0) {
            grow(p);
        }
    }
    for r in &plan.rooms {
        r.points.iter().copied().for_each(&mut grow);
    }
    (lo, hi)
}

/// A plan from selected drawing geometry: lines and open polylines become
/// wall centre lines, closed polylines become rooms.
pub fn from_geometry(
    segments: &[([f64; 2], [f64; 2])],
    polygons: &[Vec<[f64; 2]>],
    thickness: f64,
    units: &str,
) -> Plan {
    Plan {
        units: units.to_string(),
        wall_height: 0.0,
        walls: segments
            .iter()
            .filter(|(a, b)| len(sub(*b, *a)) > thickness * 0.5)
            .map(|(a, b)| Wall {
                a: *a,
                b: *b,
                thickness,
            })
            .collect(),
        openings: Vec::new(),
        rooms: polygons
            .iter()
            .enumerate()
            .map(|(n, p)| Room {
                name: format!("Room {}", n + 1),
                points: p.clone(),
            })
            .collect(),
        notes: String::new(),
    }
}

// ── Output ──────────────────────────────────────────────────────────────────

fn at(p: [f64; 2], o: [f64; 2]) -> [f64; 2] {
    add(p, o)
}

fn trim(x: f64, places: usize) -> String {
    let s = format!("{x:.places$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Spans of `wall` not taken by openings, as `(s0, s1)` distances, with the
/// ends pushed out by half the thickness so corners close.
fn solid_spans(plan: &Plan, wall: usize) -> Vec<(f64, f64)> {
    let w = &plan.walls[wall];
    let mut cuts: Vec<(f64, f64)> = plan
        .openings
        .iter()
        .filter(|o| o.wall == wall)
        .map(|o| (o.at - o.width / 2.0, o.at + o.width / 2.0))
        .collect();
    cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut s = -w.thickness / 2.0;
    for (c0, c1) in cuts {
        if c0 > s + 1e-9 {
            out.push((s, c0));
        }
        s = c1;
    }
    let end = w.length() + w.thickness / 2.0;
    if end > s + 1e-9 {
        out.push((s, end));
    }
    out
}

/// Text height for labels in drawing units: 250 mm on paper-scale plans
/// when the unit is known, else a fiftieth of the plan's size.
pub fn label_height(plan: &Plan, mm_per_unit: Option<f64>) -> f64 {
    let (lo, hi) = extents(plan);
    let size = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1e-9);
    match mm_per_unit {
        Some(mm) => (250.0 / mm).min(size / 20.0),
        None => size / 50.0,
    }
}

/// Room area in m² when the unit is known (else drawing units²).
pub fn area_label(area: f64, mm_per_unit: Option<f64>) -> String {
    match mm_per_unit {
        Some(mm) => format!("{} m²", trim(area * (mm / 1000.0).powi(2), 2)),
        None => format!("{} sq units", trim(area, 2)),
    }
}

fn length_label(l: f64, mm_per_unit: Option<f64>) -> String {
    match mm_per_unit {
        Some(mm) => format!("{} m", trim(l * mm / 1000.0, 2)),
        None => trim(l, 2),
    }
}

/// `entities_create` steps for the 2D plan, its room labels and schedule.
pub fn plan_2d(
    plan: &Plan,
    origin: [f64; 2],
    mm_per_unit: Option<f64>,
    schedule: bool,
) -> Vec<Value> {
    let mut walls = Vec::new();
    let mut doors = Vec::new();
    let mut glazing = Vec::new();
    for (n, w) in plan.walls.iter().enumerate() {
        for (s0, s1) in solid_spans(plan, n) {
            let band: Vec<[f64; 2]> = w.band(s0, s1).into_iter().map(|p| at(p, origin)).collect();
            walls.push(json!({"type":"Hatch","boundary":band,"solid":true,"layer":L_WALL}));
        }
    }
    for o in &plan.openings {
        let w = &plan.walls[o.wall];
        let (u, nrm) = w.frame();
        let h = w.thickness / 2.0;
        let (s0, s1) = (o.at - o.width / 2.0, o.at + o.width / 2.0);
        let on = |s: f64, off: f64| at(add(add(w.a, mul(u, s)), mul(nrm, off)), origin);
        match o.kind {
            OpeningKind::Door => {
                let side = if o.swing_left { 1.0 } else { -1.0 };
                let (hinge_s, free_s) = if o.hinge_start { (s0, s1) } else { (s1, s0) };
                let face = side * h;
                let hinge = on(hinge_s, face);
                let closed = on(free_s, face);
                let open = add(hinge, mul(nrm, side * o.width));
                doors.push(json!({"type":"Line","start":hinge,"end":open,"layer":L_DOOR}));
                // The swing: a quarter circle from the open leaf to the
                // closed position, counter-clockwise.
                let ang = |p: [f64; 2]| {
                    let d = sub(p, hinge);
                    d[1].atan2(d[0]).to_degrees()
                };
                let (a_open, a_closed) = (ang(open), ang(closed));
                let ccw = |from: f64, to: f64| (to - from).rem_euclid(360.0);
                let (start, end) = if ccw(a_open, a_closed) <= 180.0 {
                    (a_open, a_closed)
                } else {
                    (a_closed, a_open)
                };
                doors.push(json!({"type":"Arc","center":hinge,"radius":o.width,"start_angle_deg":start,"end_angle_deg":end,"layer":L_DOOR}));
            }
            OpeningKind::Window => {
                for off in [-h, 0.0, h] {
                    glazing.push(
                        json!({"type":"Line","start":on(s0, off),"end":on(s1, off),"layer":L_GLAZ}),
                    );
                }
                for s in [s0, s1] {
                    glazing.push(
                        json!({"type":"Line","start":on(s, -h),"end":on(s, h),"layer":L_GLAZ}),
                    );
                }
            }
        }
    }
    let th = label_height(plan, mm_per_unit);
    let mut rooms = Vec::new();
    let mut labels = Vec::new();
    for r in &plan.rooms {
        let pts: Vec<[f64; 2]> = r.points.iter().map(|p| at(*p, origin)).collect();
        rooms.push(json!({"type":"LwPolyline","vertices":pts,"closed":true,"layer":L_AREA}));
        let (area, _) = area_perimeter(&r.points);
        let c = at(centroid(&r.points), origin);
        labels.push(json!({"type":"Text","value":r.name,"position":[c[0], c[1] + th * 0.2],"height":th,"layer":L_IDEN}));
        labels.push(json!({"type":"Text","value":area_label(area, mm_per_unit),"position":[c[0], c[1] - th * 1.3],"height":th * 0.75,"layer":L_IDEN}));
    }
    let mut steps = Vec::new();
    let mut push = |entities: Vec<Value>, note: &str| {
        if !entities.is_empty() {
            steps.push(json!({"op":"entities_create","entities":entities,"note":note}));
        }
    };
    push(walls, "walls (A-WALL)");
    push(doors, "doors (A-DOOR)");
    push(glazing, "windows (A-GLAZ)");
    push(rooms, "room boundaries (A-AREA)");
    push(labels, "room names and areas (A-AREA-IDEN)");
    if schedule && !plan.rooms.is_empty() {
        let (lo, hi) = extents(plan);
        push(
            schedule_entities(
                plan,
                [origin[0] + hi[0] + th * 4.0, origin[1] + hi[1]],
                th,
                mm_per_unit,
                lo,
            ),
            "room schedule (A-ANNO-SCHD)",
        );
    }
    steps
}

/// A ruled room schedule (name, area, perimeter, total) with its top-left
/// corner at `top_left`.
fn schedule_entities(
    plan: &Plan,
    top_left: [f64; 2],
    th: f64,
    mm: Option<f64>,
    _lo: [f64; 2],
) -> Vec<Value> {
    let row = th * 2.0;
    let cols = [th * 12.0, th * 7.0, th * 7.0];
    let width: f64 = cols.iter().sum();
    let mut rows: Vec<[String; 3]> = vec![["ROOM".into(), "AREA".into(), "PERIMETER".into()]];
    let mut total = 0.0;
    for r in &plan.rooms {
        let (a, p) = area_perimeter(&r.points);
        total += a;
        rows.push([r.name.clone(), area_label(a, mm), length_label(p, mm)]);
    }
    rows.push(["TOTAL".into(), area_label(total, mm), String::new()]);
    let mut e = Vec::new();
    let [x0, y0] = top_left;
    let title_y = y0 + row * 0.6;
    e.push(json!({"type":"Text","value":"ROOM SCHEDULE","position":[x0, title_y],"height":th * 1.2,"layer":L_SCHD}));
    let n = rows.len();
    for k in 0..=n {
        let y = y0 - row * k as f64;
        e.push(json!({"type":"Line","start":[x0, y],"end":[x0 + width, y],"layer":L_SCHD}));
    }
    let mut x = x0;
    for c in 0..=cols.len() {
        e.push(
            json!({"type":"Line","start":[x, y0],"end":[x, y0 - row * n as f64],"layer":L_SCHD}),
        );
        if c < cols.len() {
            x += cols[c];
        }
    }
    for (k, cells) in rows.iter().enumerate() {
        let y = y0 - row * (k as f64 + 1.0) + row * 0.3;
        let mut x = x0;
        for (c, text) in cells.iter().enumerate() {
            if !text.is_empty() {
                e.push(json!({"type":"Text","value":text,"position":[x + th * 0.4, y],"height":th * 0.8,"layer":L_SCHD}));
            }
            x += cols[c];
        }
    }
    e
}

/// Massing solids: every solid stretch of wall, and the wall above doors and
/// above / below windows, extruded to its height on layer `A-WALL-3D`.
pub fn plan_3d(plan: &Plan, origin: [f64; 2], wall_height: f64, restore_layer: &str) -> Vec<Value> {
    let mut pieces: Vec<(Vec<[f64; 2]>, f64, f64)> = Vec::new(); // (footprint, z0, z1)
    for (n, w) in plan.walls.iter().enumerate() {
        for (s0, s1) in solid_spans(plan, n) {
            pieces.push((w.band(s0, s1), 0.0, wall_height));
        }
        for o in plan.openings.iter().filter(|o| o.wall == n) {
            let band = w.band(o.at - o.width / 2.0, o.at + o.width / 2.0);
            let head = (o.sill + o.height).min(wall_height);
            if o.kind == OpeningKind::Window && o.sill > 1e-9 {
                pieces.push((band.clone(), 0.0, o.sill));
            }
            if wall_height - head > 1e-9 {
                pieces.push((band, head, wall_height));
            }
        }
    }
    let mut steps = vec![
        json!({"op":"entities_create","entities":[{"type":"Point","location":[origin[0], origin[1], 0.0],"layer":L_3D}],"bind":"anchor","note":"3D massing"}),
        json!({"op":"sysvar","set":{"clayer":L_3D}}),
        json!({"op":"entities_delete","handles":"$anchor"}),
    ];
    for (k, (band, z0, z1)) in pieces.iter().enumerate() {
        let profile = format!("w{k}_profile");
        let solid = format!("w{k}");
        let pts: Vec<[f64; 2]> = band.iter().map(|p| at(*p, origin)).collect();
        steps.push(json!({"op":"entities_create","entities":[{"type":"LwPolyline","vertices":pts,"closed":true,"layer":L_3D}],"bind":profile}));
        steps.push(json!({"op":"start","cmd":"EXTRUDE"}));
        steps.push(json!({"op":"input","kind":"selection","handles":format!("${profile}")}));
        steps.push(json!({"op":"input","kind":"enter"}));
        steps.push(json!({"op":"input","kind":"text","text":trim(z1 - z0, 6),"bind":solid}));
        if *z0 > 1e-9 {
            steps.push(json!({"op":"entities_transform","handles":format!("${solid}"),"action":"move","vector":[0.0, 0.0, z0]}));
        }
    }
    steps.push(json!({"op":"sysvar","set":{"clayer":restore_layer},"note":format!("{} wall solid(s)", pieces.len())}));
    steps
}

fn shared_schema(units: &str) -> String {
    format!(
        r#"Answer with ONE JSON object and nothing else:
{{"units": "{units}", "wall_height": <storey height>, "wall_thickness": <default>,
  "notes": "<assumptions>",
  "walls": [{{"a": [x, y], "b": [x, y], "t": <thickness>}}, ...],
  "openings": [{{"kind": "door" | "window", "wall": <wall index>, "at": <distance from the wall's a end to the opening centre>, "width": .., "height": .., "sill": <windows only>, "swing": "in-left" | "in-right" | "out-left" | "out-right"}}],
  "rooms": [{{"name": "...", "points": [[x, y], ...]}}]}}

Walls are CENTRE LINES; where walls meet, their centre lines meet at the same point (no gaps, no overlaps). Exterior walls are typically 0.25–0.35 m, interior 0.10–0.15 m. Doors 0.8–0.9 m wide and 2.1 m high; windows have a sill (about 0.9 m) and a height (about 1.2 m). Every opening must fit inside its wall. Room points run along the wall centre lines around the room, counter-clockwise. x to the right, y up, origin at the lower-left corner of the building."#
    )
}

pub fn image_prompt(units: &str, width_px: u32, height_px: u32) -> String {
    format!(
        "You convert architectural floor-plan images ({width_px}×{height_px} px) into CAD data. Read the dimension strings and scale to get real sizes; if there are none, assume typical sizes and say so in notes. Trace every wall, door and window and name every room as labelled.\n\n{}",
        shared_schema(units)
    )
}

pub fn brief_prompt(units: &str) -> String {
    format!(
        "You are an architect laying out a single-storey floor plan from a brief. Give rooms sensible proportions and sizes for their use, a logical circulation (hall or corridor where needed), an entrance door, doors to every room and windows on exterior walls of habitable rooms. Keep the footprint rectangular or an L/T shape unless asked otherwise.\nIf a plan is given, return the whole plan with the requested changes.\n\n{}",
        shared_schema(units)
    )
}

/// The plan back as JSON (for refining with the model).
pub fn to_json(plan: &Plan) -> Value {
    json!({
        "units": plan.units,
        "wall_height": plan.wall_height,
        "notes": plan.notes,
        "walls": plan.walls.iter().map(|w| json!({"a": w.a, "b": w.b, "t": w.thickness})).collect::<Vec<_>>(),
        "openings": plan.openings.iter().map(|o| json!({
            "kind": if o.kind == OpeningKind::Door { "door" } else { "window" },
            "wall": o.wall, "at": o.at, "width": o.width, "height": o.height, "sill": o.sill,
            "swing": format!("{}-{}", if o.swing_left { "in" } else { "out" }, if o.hinge_start { "left" } else { "right" }),
        })).collect::<Vec<_>>(),
        "rooms": plan.rooms.iter().map(|r| json!({"name": r.name, "points": r.points})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10 × 6 m two-room house: four exterior walls (0.3), one partition
    /// (0.1) at x = 6, an entrance door, an internal door and a window.
    const HOUSE: &str = r#"{"units":"m","wall_height":2.7,"wall_thickness":0.3,
      "walls":[{"a":[0,0],"b":[10,0]},{"a":[10,0],"b":[10,6]},{"a":[10,6],"b":[0,6]},{"a":[0,6],"b":[0,0]},
               {"a":[6,0],"b":[6,6],"t":0.1}],
      "openings":[{"kind":"door","wall":0,"at":2,"width":0.9},
                  {"kind":"door","wall":4,"at":3,"width":0.8,"swing":"in-right"},
                  {"kind":"window","wall":2,"at":7,"width":1.5,"sill":0.9,"height":1.2}],
      "rooms":[{"name":"Living","points":[[0,0],[6,0],[6,6],[0,6]]},{"name":"Bedroom","points":[[6,0],[10,0],[10,6],[6,6],[6,0]]}]}"#;

    #[test]
    fn parses_and_checks_a_house() {
        let p = parse_plan(HOUSE).unwrap();
        assert_eq!(p.walls.len(), 5);
        assert_eq!(p.walls[4].thickness, 0.1);
        assert_eq!(p.openings[0].height, 2.1, "door height default");
        assert_eq!(p.rooms[1].points.len(), 4, "closing point dropped");
        assert_eq!(area_perimeter(&p.rooms[0].points), (36.0, 24.0));
        assert_eq!(centroid(&p.rooms[1].points), [8.0, 3.0]);
    }

    #[test]
    fn rejects_openings_that_do_not_fit_or_overlap() {
        let mut p = parse_plan(HOUSE).unwrap();
        p.openings[0].at = 9.8;
        assert!(check(&p).unwrap_err().contains("does not fit"));
        p.openings[0].at = 7.2;
        p.openings[0].wall = 2;
        assert!(check(&p).unwrap_err().contains("overlap"));
        assert!(parse_plan(r#"{"walls":[{"a":[0,0],"b":[0.1,0],"t":0.3}]}"#).is_err());
        assert_eq!(
            parse_plan(r#"{"message":"not a plan","walls":[]}"#).unwrap_err(),
            "not a plan"
        );
    }

    #[test]
    fn walls_are_split_around_openings_and_corners_close() {
        let p = parse_plan(HOUSE).unwrap();
        let spans = solid_spans(&p, 0);
        assert_eq!(spans, vec![(-0.15, 1.55), (2.45, 10.15)]);
        let steps = plan_2d(&p, [0.0, 0.0], Some(1000.0), true);
        let walls = &steps[0]["entities"];
        // 3 doors/windows split 3 walls; 5 walls + 3 extra pieces.
        assert_eq!(walls.as_array().unwrap().len(), 8);
        assert_eq!(walls[0]["layer"], L_WALL);
        let layers: Vec<&str> = steps
            .iter()
            .filter_map(|s| s["entities"][0]["layer"].as_str())
            .collect();
        assert_eq!(layers, vec![L_WALL, L_DOOR, L_GLAZ, L_AREA, L_IDEN, L_SCHD]);
        // The plan passes the AICMD plan checks.
        assert!(crate::ai::command_plan::parse_plan(&json!({"steps": steps}).to_string()).is_ok());
    }

    #[test]
    fn door_swing_is_a_quarter_circle_from_the_hinge() {
        let p = parse_plan(HOUSE).unwrap();
        let steps = plan_2d(&p, [0.0, 0.0], Some(1000.0), false);
        let doors = steps[1]["entities"].as_array().unwrap();
        let arc = &doors[1];
        assert_eq!(arc["type"], "Arc");
        assert_eq!(arc["radius"], 0.9);
        let (s, e) = (
            arc["start_angle_deg"].as_f64().unwrap(),
            arc["end_angle_deg"].as_f64().unwrap(),
        );
        assert!(((e - s).rem_euclid(360.0) - 90.0).abs() < 1e-9, "{s} → {e}");
        // Hinge on the inner face of the south wall, at the door's start.
        assert_eq!(arc["center"], json!([1.55, 0.15]));
    }

    #[test]
    fn schedule_lists_rooms_and_total_in_square_metres() {
        let p = parse_plan(HOUSE).unwrap();
        let steps = plan_2d(&p, [0.0, 0.0], Some(1000.0), true);
        let texts: Vec<String> = steps.last().unwrap()["entities"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["value"].as_str().map(str::to_string))
            .collect();
        assert!(texts.contains(&"Living".to_string()));
        assert!(texts.contains(&"36 m²".to_string()));
        assert!(texts.contains(&"60 m²".to_string()), "total {texts:?}");
        assert!(texts.contains(&"24 m".to_string()), "perimeter");
        // In millimetres the area label still reads square metres.
        let mut mm = p.clone();
        scale(&mut mm, 1000.0);
        assert_eq!(
            area_label(area_perimeter(&mm.rooms[0].points).0, Some(1.0)),
            "36 m²"
        );
    }

    #[test]
    fn massing_has_lintels_and_sills() {
        let p = parse_plan(HOUSE).unwrap();
        let steps = plan_3d(&p, [0.0, 0.0], 2.7, "0");
        let extrudes = steps.iter().filter(|s| s["op"] == "start").count();
        // 8 full-height pieces + 2 door lintels + window below and above.
        assert_eq!(extrudes, 8 + 2 + 2);
        assert_eq!(steps.last().unwrap()["set"]["clayer"], "0");
    }

    #[test]
    fn from_geometry_and_round_trip() {
        let p = from_geometry(
            &[([0.0, 0.0], [5.0, 0.0]), ([0.0, 0.0], [0.05, 0.0])],
            &[vec![[0.0, 0.0], [5.0, 0.0], [5.0, 4.0]]],
            0.2,
            "m",
        );
        assert_eq!(p.walls.len(), 1, "too-short segment dropped");
        assert_eq!(p.rooms[0].name, "Room 1");
        let house = parse_plan(HOUSE).unwrap();
        let again = parse_plan(&to_json(&house).to_string()).unwrap();
        assert_eq!(again, house);
    }
}
