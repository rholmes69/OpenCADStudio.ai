//! Constraint inference (AICONSTRAIN): the model proposes design-intent
//! relations and a dimension scheme for a sketch; every proposal is checked
//! against the actual geometry before it is offered.
//!
//! The model chooses *which* constraints express the intent ("these four
//! holes are equal", "the slot is symmetric about the centre line", "dimension
//! the overall width and the hole spacing"). It never chooses values:
//! geometric relations are only accepted when the geometry already satisfies
//! them within tolerance, and dimensional constraints take the measured
//! value. Applying the suggestions therefore never moves anything.

use serde_json::{json, Value};

/// A sketch primitive, as the model sees it and the checks measure it.
#[derive(Debug, Clone, PartialEq)]
pub enum Geom {
    Line {
        a: [f64; 2],
        b: [f64; 2],
    },
    Circle {
        c: [f64; 2],
        r: f64,
    },
    Arc {
        c: [f64; 2],
        r: f64,
        a: [f64; 2],
        b: [f64; 2],
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prim {
    /// Hex handle of the owning object.
    pub handle: String,
    /// The straight segment index when the primitive is part of a polyline.
    pub segment: Option<usize>,
    /// Vertex count of that polyline (to wrap the segment's end vertex).
    pub vertices: usize,
    pub geom: Geom,
}

/// Which part of a primitive a constraint refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Whole,
    Start,
    End,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RefSpec {
    pub prim: usize,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Horizontal,
    Vertical,
    Parallel,
    Perpendicular,
    Equal,
    Concentric,
    Coincident,
    Tangent,
    Symmetric,
    Fixed,
    /// A line's length, or the distance between two points.
    Distance,
    DistanceX,
    DistanceY,
    Radius,
    Diameter,
}

impl Kind {
    pub fn is_dimensional(self) -> bool {
        matches!(
            self,
            Kind::Distance | Kind::DistanceX | Kind::DistanceY | Kind::Radius | Kind::Diameter
        )
    }

    fn parse(s: &str) -> Option<Kind> {
        Some(
            match s
                .trim()
                .to_ascii_lowercase()
                .replace([' ', '-'], "_")
                .as_str()
            {
                "horizontal" => Kind::Horizontal,
                "vertical" => Kind::Vertical,
                "parallel" => Kind::Parallel,
                "perpendicular" => Kind::Perpendicular,
                "equal" => Kind::Equal,
                "concentric" => Kind::Concentric,
                "coincident" => Kind::Coincident,
                "tangent" => Kind::Tangent,
                "symmetric" => Kind::Symmetric,
                "fixed" | "fix" => Kind::Fixed,
                "distance" | "length" | "linear" | "aligned" => Kind::Distance,
                "distance_x" | "horizontal_distance" | "dx" => Kind::DistanceX,
                "distance_y" | "vertical_distance" | "dy" => Kind::DistanceY,
                "radius" => Kind::Radius,
                "diameter" => Kind::Diameter,
                _ => return None,
            },
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Horizontal => "Horizontal",
            Kind::Vertical => "Vertical",
            Kind::Parallel => "Parallel",
            Kind::Perpendicular => "Perpendicular",
            Kind::Equal => "Equal",
            Kind::Concentric => "Concentric",
            Kind::Coincident => "Coincident",
            Kind::Tangent => "Tangent",
            Kind::Symmetric => "Symmetric",
            Kind::Fixed => "Fix",
            Kind::Distance => "Distance",
            Kind::DistanceX => "Horizontal distance",
            Kind::DistanceY => "Vertical distance",
            Kind::Radius => "Radius",
            Kind::Diameter => "Diameter",
        }
    }
}

/// A checked suggestion, ready to apply.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub kind: Kind,
    pub refs: Vec<RefSpec>,
    /// The measured value for a dimensional constraint.
    pub value: Option<f64>,
    pub why: String,
}

/// A proposal the geometry did not support, kept for the review list.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    pub text: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    pub distance: f64,
    pub angle_rad: f64,
}

// ── Geometry helpers ────────────────────────────────────────────────────────

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn len(v: [f64; 2]) -> f64 {
    v[0].hypot(v[1])
}
fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    len(sub(a, b))
}

fn point(p: &Prim, role: Role) -> Option<[f64; 2]> {
    match (&p.geom, role) {
        (Geom::Line { a, .. }, Role::Start) | (Geom::Arc { a, .. }, Role::Start) => Some(*a),
        (Geom::Line { b, .. }, Role::End) | (Geom::Arc { b, .. }, Role::End) => Some(*b),
        (Geom::Circle { c, .. }, Role::Center) | (Geom::Arc { c, .. }, Role::Center) => Some(*c),
        _ => None,
    }
}

fn dir(p: &Prim) -> Option<[f64; 2]> {
    match p.geom {
        Geom::Line { a, b } => {
            let d = sub(b, a);
            let l = len(d);
            (l > 1e-12).then(|| [d[0] / l, d[1] / l])
        }
        _ => None,
    }
}

fn radius(p: &Prim) -> Option<f64> {
    match p.geom {
        Geom::Circle { r, .. } | Geom::Arc { r, .. } => Some(r),
        _ => None,
    }
}

fn line_len(p: &Prim) -> Option<f64> {
    match p.geom {
        Geom::Line { a, b } => Some(dist(a, b)),
        _ => None,
    }
}

/// Reflect `p` across the infinite line through `a`–`b`.
fn reflect(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    let d = sub(b, a);
    let l2 = d[0] * d[0] + d[1] * d[1];
    let t = ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / l2;
    let foot = [a[0] + t * d[0], a[1] + t * d[1]];
    [2.0 * foot[0] - p[0], 2.0 * foot[1] - p[1]]
}

fn point_line_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = sub(b, a);
    ((p[0] - a[0]) * d[1] - (p[1] - a[1]) * d[0]).abs() / len(d).max(1e-12)
}

// ── Parsing the model's proposals ───────────────────────────────────────────

fn parse_ref(v: &Value, n: usize) -> Option<RefSpec> {
    let (prim, role) = match v {
        Value::Number(x) => (x.as_u64()? as usize, Role::Whole),
        Value::String(s) => {
            let (id, role) = s.split_once('.').unwrap_or((s.as_str(), "whole"));
            let role = match role.trim().to_ascii_lowercase().as_str() {
                "start" | "a" | "p0" => Role::Start,
                "end" | "b" | "p1" => Role::End,
                "center" | "centre" | "c" => Role::Center,
                _ => Role::Whole,
            };
            (id.trim().trim_start_matches('#').parse().ok()?, role)
        }
        _ => return None,
    };
    (prim < n).then_some(RefSpec { prim, role })
}

fn describe_ref(r: RefSpec) -> String {
    match r.role {
        Role::Whole => format!("#{}", r.prim),
        Role::Start => format!("#{}.start", r.prim),
        Role::End => format!("#{}.end", r.prim),
        Role::Center => format!("#{}.center", r.prim),
    }
}

pub fn describe(s: &Suggestion) -> String {
    let refs = s
        .refs
        .iter()
        .map(|r| describe_ref(*r))
        .collect::<Vec<_>>()
        .join(", ");
    let value = s
        .value
        .map(|v| format!(" = {}", trim(v)))
        .unwrap_or_default();
    let why = if s.why.is_empty() {
        String::new()
    } else {
        format!(" — {}", s.why)
    };
    format!("{} {refs}{value}{why}", s.kind.label())
}

fn trim(x: f64) -> String {
    let s = format!("{x:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Check one proposal against the geometry. `Ok` carries the measured value
/// for dimensional kinds.
pub fn check(
    kind: Kind,
    refs: &[RefSpec],
    prims: &[Prim],
    tol: Tolerance,
) -> Result<Option<f64>, String> {
    let p = |i: usize| &prims[refs[i].prim];
    let need = |n: usize| {
        if refs.len() == n {
            Ok(())
        } else {
            Err(format!("{} takes {n} reference(s)", kind.label()))
        }
    };
    let whole_line = |i: usize| -> Result<[f64; 2], String> {
        if refs[i].role != Role::Whole {
            return Err("expects a whole line".into());
        }
        dir(p(i)).ok_or_else(|| format!("#{} is not a line", refs[i].prim))
    };
    let pt = |i: usize| -> Result<[f64; 2], String> {
        point(p(i), refs[i].role).ok_or_else(|| format!("{} is not a point", describe_ref(refs[i])))
    };
    let parallel =
        |u: [f64; 2], v: [f64; 2]| (u[0] * v[1] - u[1] * v[0]).abs() <= tol.angle_rad.sin();
    let held = |ok: bool, what: &str| {
        if ok {
            Ok(None)
        } else {
            Err(format!("the geometry is not {what}"))
        }
    };
    match kind {
        Kind::Horizontal | Kind::Vertical => {
            need(1)?;
            let d = whole_line(0)?;
            if kind == Kind::Horizontal {
                held(d[1].abs() <= tol.angle_rad.sin(), "horizontal")
            } else {
                held(d[0].abs() <= tol.angle_rad.sin(), "vertical")
            }
        }
        Kind::Parallel | Kind::Perpendicular => {
            need(2)?;
            let (u, v) = (whole_line(0)?, whole_line(1)?);
            if kind == Kind::Parallel {
                held(parallel(u, v), "parallel")
            } else {
                held(
                    (u[0] * v[0] + u[1] * v[1]).abs() <= tol.angle_rad.sin(),
                    "perpendicular",
                )
            }
        }
        Kind::Equal => {
            need(2)?;
            if refs.iter().any(|r| r.role != Role::Whole) {
                return Err("Equal relates whole objects".into());
            }
            match (line_len(p(0)), line_len(p(1)), radius(p(0)), radius(p(1))) {
                (Some(a), Some(b), _, _) => held((a - b).abs() <= tol.distance, "of equal length"),
                (_, _, Some(a), Some(b)) => held((a - b).abs() <= tol.distance, "of equal radius"),
                _ => Err("Equal needs two lines or two circles/arcs".into()),
            }
        }
        Kind::Concentric => {
            need(2)?;
            let (a, b) = (
                radius(p(0)).and(point(p(0), Role::Center)),
                radius(p(1)).and(point(p(1), Role::Center)),
            );
            match (a, b) {
                (Some(a), Some(b)) => held(dist(a, b) <= tol.distance, "concentric"),
                _ => Err("Concentric needs two circles/arcs".into()),
            }
        }
        Kind::Coincident => {
            need(2)?;
            held(dist(pt(0)?, pt(1)?) <= tol.distance, "coincident")
        }
        Kind::Tangent => {
            need(2)?;
            let (a, b) = (p(0), p(1));
            let ok = match (&a.geom, &b.geom) {
                (Geom::Line { a: l0, b: l1 }, g) | (g, Geom::Line { a: l0, b: l1 })
                    if !matches!(g, Geom::Line { .. }) =>
                {
                    let (c, r) = match g {
                        Geom::Circle { c, r } | Geom::Arc { c, r, .. } => (*c, *r),
                        Geom::Line { .. } => unreachable!(),
                    };
                    (point_line_distance(c, *l0, *l1) - r).abs() <= tol.distance
                }
                (Geom::Line { .. }, Geom::Line { .. }) => {
                    return Err("two lines cannot be tangent".into())
                }
                _ => {
                    let (c0, r0, c1, r1) = (
                        point(a, Role::Center).unwrap(),
                        radius(a).unwrap(),
                        point(b, Role::Center).unwrap(),
                        radius(b).unwrap(),
                    );
                    let d = dist(c0, c1);
                    (d - (r0 + r1)).abs() <= tol.distance
                        || (d - (r0 - r1).abs()).abs() <= tol.distance
                }
            };
            held(ok, "tangent")
        }
        Kind::Symmetric => {
            need(3)?;
            if refs[2].role != Role::Whole {
                return Err("the symmetry axis must be a whole line".into());
            }
            let Geom::Line { a: ax, b: bx } = p(2).geom else {
                return Err("the symmetry axis must be a line".into());
            };
            let close = |u: [f64; 2], v: [f64; 2]| dist(u, v) <= tol.distance;
            let ok = match (refs[0].role, refs[1].role) {
                (Role::Whole, Role::Whole) => match (&p(0).geom, &p(1).geom) {
                    (Geom::Line { a: a0, b: b0 }, Geom::Line { a: a1, b: b1 }) => {
                        let (m0, m1) = (reflect(*a0, ax, bx), reflect(*b0, ax, bx));
                        (close(m0, *a1) && close(m1, *b1)) || (close(m0, *b1) && close(m1, *a1))
                    }
                    (
                        Geom::Circle { c: c0, r: r0 } | Geom::Arc { c: c0, r: r0, .. },
                        Geom::Circle { c: c1, r: r1 } | Geom::Arc { c: c1, r: r1, .. },
                    ) => close(reflect(*c0, ax, bx), *c1) && (r0 - r1).abs() <= tol.distance,
                    _ => return Err("symmetric objects must be the same kind".into()),
                },
                (Role::Whole, _) | (_, Role::Whole) => {
                    return Err("mix of points and objects".into())
                }
                _ => close(reflect(pt(0)?, ax, bx), pt(1)?),
            };
            held(ok, "symmetric about that line")
        }
        Kind::Fixed => {
            need(1)?;
            Ok(None)
        }
        Kind::Distance => match refs.len() {
            1 => line_len(p(0))
                .filter(|_| refs[0].role == Role::Whole)
                .map(Some)
                .ok_or_else(|| "a single-reference distance needs a whole line".into()),
            2 => Ok(Some(dist(pt(0)?, pt(1)?))),
            _ => Err("Distance takes a line or two points".into()),
        },
        Kind::DistanceX | Kind::DistanceY => {
            need(2)?;
            let d = sub(pt(1)?, pt(0)?);
            Ok(Some(if kind == Kind::DistanceX { d[0] } else { d[1] }))
        }
        Kind::Radius | Kind::Diameter => {
            need(1)?;
            let r =
                radius(p(0)).ok_or_else(|| format!("#{} is not a circle or arc", refs[0].prim))?;
            Ok(Some(if kind == Kind::Radius { r } else { 2.0 * r }))
        }
    }
}

/// Parse and check the model's reply. Proposals that do not hold are
/// returned separately; duplicates (also of `existing`) are dropped.
pub fn parse_suggestions(
    reply: &str,
    prims: &[Prim],
    tol: Tolerance,
    existing: &[(Kind, Vec<RefSpec>)],
) -> Result<(String, Vec<Suggestion>, Vec<Rejected>), String> {
    let v = super::client::extract_json(reply).ok_or("the model did not answer with JSON")?;
    let list = v["constraints"]
        .as_array()
        .ok_or("the reply has no \"constraints\" list")?;
    let mut accepted: Vec<Suggestion> = Vec::new();
    let mut rejected = Vec::new();
    let mut seen: std::collections::HashSet<(Kind, Vec<RefSpec>)> = existing
        .iter()
        .map(|(k, r)| (*k, canonical(*k, r)))
        .collect();
    for c in list.iter().take(400) {
        let raw = c.to_string();
        let Some(kind) = c["kind"].as_str().and_then(Kind::parse) else {
            rejected.push(Rejected {
                text: raw,
                reason: "unknown constraint kind".into(),
            });
            continue;
        };
        let refs: Option<Vec<RefSpec>> = c["refs"]
            .as_array()
            .and_then(|a| a.iter().map(|r| parse_ref(r, prims.len())).collect());
        let Some(mut refs) = refs.filter(|r| !r.is_empty()) else {
            rejected.push(Rejected {
                text: raw,
                reason: "references unknown objects".into(),
            });
            continue;
        };
        let why = c["why"].as_str().unwrap_or("").trim().to_string();
        match check(kind, &refs, prims, tol) {
            Ok(mut value) => {
                // Signed X/Y distances read left-to-right / bottom-to-top.
                if let Some(x) = value.filter(|x| *x < 0.0) {
                    if matches!(kind, Kind::DistanceX | Kind::DistanceY) {
                        refs.swap(0, 1);
                        value = Some(-x);
                    }
                }
                if value.is_some_and(|x| x <= 1e-9) {
                    rejected.push(Rejected {
                        text: raw,
                        reason: "measures zero".into(),
                    });
                    continue;
                }
                if !seen.insert((kind, canonical(kind, &refs))) {
                    continue;
                }
                accepted.push(Suggestion {
                    kind,
                    refs,
                    value,
                    why,
                });
            }
            Err(reason) => rejected.push(Rejected {
                text: format!("{} {}", kind.label(), c["refs"]),
                reason,
            }),
        }
    }
    Ok((
        v["intent"].as_str().unwrap_or("").trim().to_string(),
        accepted,
        rejected,
    ))
}

/// Order-insensitive key for symmetric relations.
fn canonical(kind: Kind, refs: &[RefSpec]) -> Vec<RefSpec> {
    let mut r = refs.to_vec();
    if matches!(
        kind,
        Kind::Parallel
            | Kind::Perpendicular
            | Kind::Equal
            | Kind::Concentric
            | Kind::Coincident
            | Kind::Tangent
            | Kind::Distance
    ) {
        r.sort_by_key(|x| (x.prim, x.role as u8));
    } else if kind == Kind::Symmetric && r.len() == 3 {
        r[..2].sort_by_key(|x| (x.prim, x.role as u8));
    }
    r
}

pub fn prompt(prims: &[Prim], existing: &[(Kind, Vec<RefSpec>)], units: &str) -> String {
    let f = |p: [f64; 2]| format!("({}, {})", trim(p[0]), trim(p[1]));
    let mut listing = String::new();
    for (n, p) in prims.iter().enumerate() {
        let line = match &p.geom {
            Geom::Line { a, b } => format!(
                "#{n} line {} → {} length {}",
                f(*a),
                f(*b),
                trim(dist(*a, *b))
            ),
            Geom::Circle { c, r } => format!("#{n} circle center {} r {}", f(*c), trim(*r)),
            Geom::Arc { c, r, a, b } => format!(
                "#{n} arc center {} r {} from {} to {}",
                f(*c),
                trim(*r),
                f(*a),
                f(*b)
            ),
        };
        listing.push_str(&line);
        listing.push('\n');
    }
    let have = existing
        .iter()
        .map(|(k, r)| {
            format!(
                "{} {}",
                k.label(),
                r.iter()
                    .map(|x| describe_ref(*x))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        r#"You are a CAD design engineer adding constraints to a 2D sketch so it captures design intent and becomes fully defined. Units: {units}.

Sketch primitives:
{listing}
Constraints already found or present: {have}

Propose ADDITIONAL constraints:
1. design intent the automatic pass misses — equal sizes across a pattern (bolt circles, repeated slots), symmetry about a centre line, concentric features, a fixed anchor point;
2. a clean dimension scheme a drafter would use: overall sizes, hole sizes (diameter), hole positions from a datum (horizontal/vertical distances), no redundant or chained-to-closure dimensions.

Answer with ONE JSON object:
{{"intent": "<one sentence describing what the sketch is>",
  "constraints": [{{"kind": "<kind>", "refs": [<ref>, ...], "why": "<short reason>"}}]}}

Kinds and refs: horizontal/vertical [line]; parallel/perpendicular [line, line]; equal [obj, obj] (lines: length, circles/arcs: radius); concentric [circle, circle]; coincident [point, point]; tangent [obj, obj]; symmetric [obj, obj, axis_line] or [point, point, axis_line]; fixed [obj]; distance [line] or [point, point]; distance_x / distance_y [point, point]; radius / diameter [circle_or_arc].
A ref is a primitive number (the whole object) or "<n>.start", "<n>.end", "<n>.center". Do not give values: the current sizes are kept. Only propose relations that are already (nearly) true in the coordinates above."#
    )
}

/// The suggestion as JSON for logs and tests.
pub fn to_json(s: &Suggestion) -> Value {
    json!({"kind": s.kind.label(), "refs": s.refs.iter().map(|r| describe_ref(*r)).collect::<Vec<_>>(), "value": s.value})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(a: [f64; 2], b: [f64; 2]) -> Prim {
        Prim {
            handle: "1".into(),
            segment: None,
            vertices: 0,
            geom: Geom::Line { a, b },
        }
    }
    fn circle(c: [f64; 2], r: f64) -> Prim {
        Prim {
            handle: "2".into(),
            segment: None,
            vertices: 0,
            geom: Geom::Circle { c, r },
        }
    }
    const TOL: Tolerance = Tolerance {
        distance: 1e-3,
        angle_rad: 0.01,
    };

    /// A plate 100 × 60 with two Ø10 holes symmetric about x = 50.
    fn plate() -> Vec<Prim> {
        vec![
            line([0.0, 0.0], [100.0, 0.0]),
            line([100.0, 0.0], [100.0, 60.0]),
            line([100.0, 60.0], [0.0, 60.0]),
            line([0.0, 60.0], [0.0, 0.0]),
            circle([20.0, 30.0], 5.0),
            circle([80.0, 30.0], 5.0),
            line([50.0, -5.0], [50.0, 65.0]),
        ]
    }

    #[test]
    fn accepts_true_relations_with_measured_values() {
        let reply = r#"{"intent":"plate with two holes","constraints":[
            {"kind":"equal","refs":[4,5],"why":"same holes"},
            {"kind":"symmetric","refs":[4,5,6]},
            {"kind":"diameter","refs":[4]},
            {"kind":"distance","refs":[0]},
            {"kind":"distance_x","refs":["5.center","0.start"]},
            {"kind":"equal","refs":[5,4]}
        ]}"#;
        let (intent, ok, bad) = parse_suggestions(reply, &plate(), TOL, &[]).unwrap();
        assert_eq!(intent, "plate with two holes");
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(ok.len(), 5, "duplicate equal dropped: {ok:?}");
        assert_eq!(ok[2].value, Some(10.0));
        assert_eq!(ok[3].value, Some(100.0));
        // Negative X distance is flipped to read left to right.
        assert_eq!(ok[4].value, Some(80.0));
        assert_eq!(ok[4].refs[0].prim, 0);
    }

    #[test]
    fn rejects_relations_the_geometry_does_not_satisfy() {
        let reply = r#"{"constraints":[
            {"kind":"perpendicular","refs":[0,2]},
            {"kind":"equal","refs":[0,1]},
            {"kind":"radius","refs":[0]},
            {"kind":"symmetric","refs":[4,5,0]},
            {"kind":"spline_magic","refs":[1]},
            {"kind":"horizontal","refs":[99]}
        ]}"#;
        let (_, ok, bad) = parse_suggestions(reply, &plate(), TOL, &[]).unwrap();
        assert!(ok.is_empty(), "{ok:?}");
        assert_eq!(bad.len(), 6);
        assert!(bad[0].reason.contains("perpendicular"));
    }

    #[test]
    fn existing_constraints_are_not_repeated() {
        let existing = vec![(
            Kind::Equal,
            vec![
                RefSpec {
                    prim: 5,
                    role: Role::Whole,
                },
                RefSpec {
                    prim: 4,
                    role: Role::Whole,
                },
            ],
        )];
        let (_, ok, _) = parse_suggestions(
            r#"{"constraints":[{"kind":"equal","refs":[4,5]}]}"#,
            &plate(),
            TOL,
            &existing,
        )
        .unwrap();
        assert!(ok.is_empty());
    }

    #[test]
    fn tangent_checks() {
        let p = vec![
            line([0.0, 5.0], [10.0, 5.0]),
            circle([5.0, 0.0], 5.0),
            circle([15.0, 0.0], 5.0),
        ];
        assert!(check(
            Kind::Tangent,
            &[
                RefSpec {
                    prim: 0,
                    role: Role::Whole
                },
                RefSpec {
                    prim: 1,
                    role: Role::Whole
                }
            ],
            &p,
            TOL
        )
        .is_ok());
        assert!(check(
            Kind::Tangent,
            &[
                RefSpec {
                    prim: 1,
                    role: Role::Whole
                },
                RefSpec {
                    prim: 2,
                    role: Role::Whole
                }
            ],
            &p,
            TOL
        )
        .is_ok());
        assert!(check(
            Kind::Tangent,
            &[
                RefSpec {
                    prim: 0,
                    role: Role::Whole
                },
                RefSpec {
                    prim: 0,
                    role: Role::Whole
                }
            ],
            &p,
            TOL
        )
        .is_err());
    }

    #[test]
    fn prompt_lists_primitives() {
        let p = prompt(&plate(), &[], "mm");
        assert!(p.contains("#4 circle center (20, 30) r 5"));
        assert!(p.contains("Units: mm"));
    }
}
