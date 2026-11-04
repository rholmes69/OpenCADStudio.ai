//! Text to part (AIPART): the model writes a small parametric part
//! definition; Open CAD Studio evaluates it and builds the solid with the
//! kernel's own EXTRUDE / REVOLVE / UNION / SUBTRACT / INTERSECT.
//!
//! A definition is plain JSON:
//!
//! ```json
//! {"name": "M8 hex bolt", "units": "mm",
//!  "parameters": {"d": 8, "L": 40, "k": 5.3, "s": 13},
//!  "bodies": [
//!    {"op": "add", "prism": {"sides": 6, "across_flats": "s", "h": "k"}},
//!    {"op": "add", "cylinder": {"d": "d", "h": "L"}, "at": [0, 0, "k"]}
//!  ]}
//! ```
//!
//! Sizes are numbers or expressions over the parameters (`+ - * / ^`,
//! parentheses, `sqrt sin cos tan` in degrees, `min max abs`, `pi`). The
//! definition is stored on the finished solid as extended data, so the part
//! can be reopened later, its parameters changed and the solid rebuilt
//! without asking the model again.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub const XDATA_APP: &str = "OCS_AIPART";
pub const MAX_BODIES: usize = 64;
pub const MAX_INSTANCES: usize = 200;
pub const LAYER: &str = "AI-PART";

// ── Expressions ─────────────────────────────────────────────────────────────

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    vars: &'a BTreeMap<String, f64>,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expr(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                return Ok(v);
            }
        }
    }
    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                let d = self.unary()?;
                if d == 0.0 {
                    return Err("division by zero".into());
                }
                v /= d;
            } else {
                return Ok(v);
            }
        }
    }
    /// `-x^2` is `-(x^2)`, as in ordinary maths.
    fn unary(&mut self) -> Result<f64, String> {
        if self.eat(b'-') {
            return Ok(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.power()
    }
    fn power(&mut self) -> Result<f64, String> {
        let b = self.atom()?;
        if self.eat(b'^') {
            Ok(b.powf(self.unary()?))
        } else {
            Ok(b)
        }
    }
    fn atom(&mut self) -> Result<f64, String> {
        if self.eat(b'(') {
            let v = self.expr()?;
            if !self.eat(b')') {
                return Err("missing )".into());
            }
            return Ok(v);
        }
        let start = self.i;
        match self.peek() {
            Some(c) if c.is_ascii_digit() || c == b'.' => {
                while self.i < self.s.len()
                    && (self.s[self.i].is_ascii_digit()
                        || self.s[self.i] == b'.'
                        || self.s[self.i] == b'e'
                        || ((self.s[self.i] == b'-' || self.s[self.i] == b'+')
                            && self.s[self.i - 1] == b'e'))
                {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
                t.parse().map_err(|_| format!("bad number \"{t}\""))
            }
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                while self.i < self.s.len()
                    && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_')
                {
                    self.i += 1;
                }
                let name = std::str::from_utf8(&self.s[start..self.i])
                    .unwrap_or("")
                    .to_string();
                if self.eat(b'(') {
                    let mut args = vec![self.expr()?];
                    while self.eat(b',') {
                        args.push(self.expr()?);
                    }
                    if !self.eat(b')') {
                        return Err(format!("missing ) after {name}("));
                    }
                    let one = |f: fn(f64) -> f64| -> Result<f64, String> {
                        if args.len() == 1 {
                            Ok(f(args[0]))
                        } else {
                            Err(format!("{name} takes one argument"))
                        }
                    };
                    return match name.to_ascii_lowercase().as_str() {
                        "sqrt" => one(f64::sqrt),
                        "abs" => one(f64::abs),
                        "sin" => one(|x| x.to_radians().sin()),
                        "cos" => one(|x| x.to_radians().cos()),
                        "tan" => one(|x| x.to_radians().tan()),
                        "round" => one(f64::round),
                        "min" => Ok(args.iter().copied().fold(f64::INFINITY, f64::min)),
                        "max" => Ok(args.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
                        other => Err(format!("unknown function {other}")),
                    };
                }
                if name.eq_ignore_ascii_case("pi") {
                    return Ok(std::f64::consts::PI);
                }
                self.vars
                    .get(&name)
                    .copied()
                    .ok_or_else(|| format!("unknown parameter \"{name}\""))
            }
            _ => Err(format!(
                "unexpected \"{}\"",
                std::str::from_utf8(&self.s[self.i..])
                    .unwrap_or("")
                    .chars()
                    .next()
                    .map(String::from)
                    .unwrap_or_else(|| "end".into())
            )),
        }
    }
}

pub fn eval_expr(text: &str, vars: &BTreeMap<String, f64>) -> Result<f64, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        vars,
    };
    let v = p.expr()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("unexpected text in \"{text}\""));
    }
    if !v.is_finite() {
        return Err(format!("\"{text}\" is not a finite number"));
    }
    Ok(v)
}

fn val(v: &Value, vars: &BTreeMap<String, f64>, what: &str) -> Result<f64, String> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| format!("{what}: bad number")),
        Value::String(s) => eval_expr(s, vars).map_err(|e| format!("{what}: {e}")),
        Value::Null => Err(format!("{what} is missing")),
        _ => Err(format!("{what} must be a number or an expression")),
    }
}

fn pos(v: &Value, vars: &BTreeMap<String, f64>, what: &str) -> Result<f64, String> {
    let x = val(v, vars, what)?;
    if x > 0.0 {
        Ok(x)
    } else {
        Err(format!("{what} must be positive (is {x})"))
    }
}

// ── Definition ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyOp {
    Add,
    Cut,
    Intersect,
}

/// One evaluated solid, ready to build.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// A closed profile in the XY plane extruded `h` along +Z.
    Prism {
        profile: Vec<[f64; 2]>,
        h: f64,
    },
    Cylinder {
        r: f64,
        h: f64,
    },
    /// A half-section `(radius, z)` revolved a full turn about the Z axis.
    Revolve {
        section: Vec<[f64; 2]>,
    },
}

/// Which way a body's own Z axis points once it is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    /// Turn a point from the body's own frame (axis along Z) into the part's.
    pub fn apply(self, p: [f64; 3]) -> [f64; 3] {
        match self {
            // +90° about Y: Z → +X.
            Axis::X => [p[2], p[1], -p[0]],
            // −90° about X: Z → +Y.
            Axis::Y => [p[0], p[2], -p[1]],
            Axis::Z => p,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub op: BodyOp,
    pub shape: Shape,
    /// Where the body's own origin ends up in the part.
    pub at: [f64; 3],
    /// The body's own Z axis direction (cylinders, prisms and revolves are
    /// built along it; patterns lie in the plane across it).
    pub axis: Axis,
    /// The pattern offset in the body's own frame (`at` = base + axis(local)).
    pub local: [f64; 3],
    /// Index of the definition body this instance comes from.
    pub body: usize,
    /// Rotation about Z in degrees (from a circular pattern or `rotate`).
    pub rotate: f64,
    pub label: String,
}

/// A named size: its expression as written (a number or text over earlier
/// parameters) and its evaluated value.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub expr: Value,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub name: String,
    pub units: String,
    /// In written order; later parameters may use earlier ones.
    pub params: Vec<Param>,
    pub notes: String,
    /// The definition as written (bodies keep their expressions).
    pub source: Value,
}

/// Evaluate parameters in dependency order (JSON objects need not keep the
/// written order), refusing cycles and unknown names.
fn evaluate_params(params: &mut [Param]) -> Result<(), String> {
    let mut known: BTreeMap<String, f64> = BTreeMap::new();
    let mut pending: Vec<usize> = (0..params.len()).collect();
    while !pending.is_empty() {
        let mut progressed = false;
        let mut last_err = String::new();
        pending.retain(|&i| {
            match val(
                &params[i].expr,
                &known,
                &format!("parameter {}", params[i].name),
            ) {
                Ok(v) => {
                    params[i].value = v;
                    known.insert(params[i].name.clone(), v);
                    progressed = true;
                    false
                }
                Err(e) => {
                    last_err = e;
                    true
                }
            }
        });
        if !progressed {
            return Err(last_err);
        }
    }
    Ok(())
}

impl Part {
    pub fn vars(&self) -> BTreeMap<String, f64> {
        self.params
            .iter()
            .map(|p| (p.name.clone(), p.value))
            .collect()
    }

    /// Change one parameter to a number or an expression and re-evaluate
    /// everything that depends on it; nothing changes if it fails.
    pub fn set_param(&mut self, name: &str, text: &str) -> Result<(), String> {
        let t = text.trim();
        let expr = t
            .parse::<f64>()
            .map(|x| json!(x))
            .unwrap_or_else(|_| json!(t));
        let mut next = self.params.clone();
        let slot = next
            .iter_mut()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("no parameter {name}"))?;
        slot.expr = expr;
        evaluate_params(&mut next)?;
        let before = std::mem::replace(&mut self.params, next);
        if let Err(e) = instances(self) {
            self.params = before;
            return Err(e);
        }
        Ok(())
    }

    /// The definition with the current parameter expressions, for storing.
    pub fn to_json(&self) -> Value {
        let mut v = self.source.clone();
        let mut params = Map::new();
        for p in &self.params {
            params.insert(p.name.clone(), p.expr.clone());
        }
        v["parameters"] = Value::Object(params);
        v["name"] = json!(self.name);
        v["units"] = json!(self.units);
        v
    }
}

/// Parse a definition (from the model or from stored extended data).
pub fn parse_part(text: &str) -> Result<Part, String> {
    let v = super::client::extract_json(text).ok_or("the answer is not a JSON part definition")?;
    if let Some(m) = v["message"].as_str().filter(|m| !m.trim().is_empty()) {
        if v["bodies"].as_array().is_none_or(|b| b.is_empty()) {
            return Err(m.trim().to_string());
        }
    }
    let mut params = Vec::new();
    if let Some(p) = v["parameters"].as_object() {
        for (k, x) in p {
            if !k
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(format!(
                    "parameter name \"{k}\" must be letters, digits and _"
                ));
            }
            params.push(Param {
                name: k.clone(),
                expr: x.clone(),
                value: 0.0,
            });
        }
    }
    // A parameter may be derived from those before it.
    evaluate_params(&mut params)?;
    let part = Part {
        name: v["name"].as_str().unwrap_or("AI part").trim().to_string(),
        units: v["units"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase(),
        params,
        notes: v["notes"].as_str().unwrap_or("").trim().to_string(),
        source: v.clone(),
    };
    instances(&part)?;
    Ok(part)
}

fn polygon(sides: usize, circumradius: f64, rot_deg: f64) -> Vec<[f64; 2]> {
    (0..sides)
        .map(|k| {
            let t = (rot_deg + 360.0 * k as f64 / sides as f64).to_radians();
            [circumradius * t.cos(), circumradius * t.sin()]
        })
        .collect()
}

fn points(v: &Value, vars: &BTreeMap<String, f64>, what: &str) -> Result<Vec<[f64; 2]>, String> {
    let list = v
        .as_array()
        .ok_or_else(|| format!("{what} must be a list of [x, y] points"))?;
    list.iter()
        .enumerate()
        .map(|(n, p)| {
            let a = p
                .as_array()
                .filter(|a| a.len() >= 2)
                .ok_or_else(|| format!("{what}[{n}] must be [x, y]"))?;
            Ok([val(&a[0], vars, what)?, val(&a[1], vars, what)?])
        })
        .collect()
}

fn signed_area(p: &[[f64; 2]]) -> f64 {
    (0..p.len())
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        / 2.0
}

/// Evaluate every body (expanding patterns) with the current parameters.
pub fn instances(part: &Part) -> Result<Vec<Instance>, String> {
    let vars = part.vars();
    let bodies = part.source["bodies"]
        .as_array()
        .filter(|b| !b.is_empty())
        .ok_or("the part has no bodies")?;
    if bodies.len() > MAX_BODIES {
        return Err(format!(
            "{} bodies is more than the {MAX_BODIES} allowed",
            bodies.len()
        ));
    }
    let mut out = Vec::new();
    for (n, b) in bodies.iter().enumerate() {
        let label = b["name"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("body {}", n + 1));
        let what = |s: &str| format!("{label}: {s}");
        let op = match b["op"]
            .as_str()
            .unwrap_or("add")
            .to_ascii_lowercase()
            .as_str()
        {
            "add" | "union" | "join" => BodyOp::Add,
            "cut" | "subtract" | "hole" | "remove" => BodyOp::Cut,
            "intersect" | "common" => BodyOp::Intersect,
            other => return Err(what(&format!("unknown op \"{other}\""))),
        };
        let shape = if let Some(s) = b.get("box") {
            let (w, d, h) = (
                pos(&s["w"], &vars, &what("box w"))?,
                pos(&s["d"], &vars, &what("box d"))?,
                pos(&s["h"], &vars, &what("box h"))?,
            );
            // Centred on its position in X and Y, base at z.
            Shape::Prism {
                profile: vec![
                    [-w / 2.0, -d / 2.0],
                    [w / 2.0, -d / 2.0],
                    [w / 2.0, d / 2.0],
                    [-w / 2.0, d / 2.0],
                ],
                h,
            }
        } else if let Some(s) = b.get("cylinder") {
            let r = if s.get("d").is_some() {
                pos(&s["d"], &vars, &what("cylinder d"))? / 2.0
            } else {
                pos(&s["r"], &vars, &what("cylinder r"))?
            };
            Shape::Cylinder {
                r,
                h: pos(&s["h"], &vars, &what("cylinder h"))?,
            }
        } else if let Some(s) = b.get("prism") {
            let sides = val(&s["sides"], &vars, &what("prism sides"))?.round() as usize;
            if !(3..=64).contains(&sides) {
                return Err(what("prism sides must be 3–64"));
            }
            let half = std::f64::consts::PI / sides as f64;
            let circ = if s.get("across_flats").is_some() {
                pos(&s["across_flats"], &vars, &what("prism across_flats"))? / 2.0 / half.cos()
            } else if s.get("d").is_some() {
                pos(&s["d"], &vars, &what("prism d"))? / 2.0
            } else {
                return Err(what("prism needs across_flats or d"));
            };
            // Even-sided prisms put a flat on the X axis.
            let rot = if sides % 2 == 0 {
                180.0 / sides as f64
            } else {
                90.0
            };
            Shape::Prism {
                profile: polygon(sides, circ, rot),
                h: pos(&s["h"], &vars, &what("prism h"))?,
            }
        } else if let Some(s) = b.get("extrude") {
            let mut profile = points(&s["points"], &vars, &what("extrude points"))?;
            if profile.len() >= 2 && profile.first() == profile.last() {
                profile.pop();
            }
            if profile.len() < 3 || signed_area(&profile).abs() < 1e-12 {
                return Err(what("extrude needs a closed profile with area"));
            }
            Shape::Prism {
                profile,
                h: pos(&s["h"], &vars, &what("extrude h"))?,
            }
        } else if let Some(s) = b.get("revolve") {
            let mut section = points(&s["points"], &vars, &what("revolve points"))?;
            if section.len() >= 2 && section.first() == section.last() {
                section.pop();
            }
            if section.len() < 3 || signed_area(&section).abs() < 1e-12 {
                return Err(what("revolve needs a closed half-section with area"));
            }
            if section.iter().any(|p| p[0] < -1e-9) {
                return Err(what("revolve points are (radius, z) with radius ≥ 0"));
            }
            Shape::Revolve { section }
        } else {
            return Err(what(
                "needs one of box, cylinder, prism, extrude or revolve",
            ));
        };
        let at = match b.get("at") {
            Some(Value::Array(a)) if a.len() >= 2 => [
                val(&a[0], &vars, &what("at x"))?,
                val(&a[1], &vars, &what("at y"))?,
                a.get(2)
                    .map(|z| val(z, &vars, &what("at z")))
                    .transpose()?
                    .unwrap_or(0.0),
            ],
            None | Some(Value::Null) => [0.0, 0.0, 0.0],
            Some(_) => return Err(what("at must be [x, y] or [x, y, z]")),
        };
        let axis = match b
            .get("axis")
            .and_then(Value::as_str)
            .unwrap_or("z")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "z" | "+z" | "" => Axis::Z,
            "x" | "+x" => Axis::X,
            "y" | "+y" => Axis::Y,
            other => return Err(what(&format!("axis \"{other}\" must be x, y or z"))),
        };
        let rotate = b
            .get("rotate")
            .map(|r| val(r, &vars, &what("rotate")))
            .transpose()?
            .unwrap_or(0.0);
        // Patterns.
        // Pattern offsets in the body's own frame, then tilted onto its axis.
        let mut placements: Vec<([f64; 3], f64)> = vec![([0.0; 3], rotate)];
        if let Some(p) = b.get("pattern") {
            if let Some(c) = p.get("circular") {
                let count = val(&c["count"], &vars, &what("pattern count"))?.round() as usize;
                let radius = val(&c["radius"], &vars, &what("pattern radius"))?;
                let start = c
                    .get("start_deg")
                    .map(|s| val(s, &vars, &what("pattern start_deg")))
                    .transpose()?
                    .unwrap_or(0.0);
                let sweep = c
                    .get("sweep_deg")
                    .map(|s| val(s, &vars, &what("pattern sweep_deg")))
                    .transpose()?
                    .unwrap_or(360.0);
                if count == 0 {
                    return Err(what("pattern count must be at least 1"));
                }
                let step = if (sweep - 360.0).abs() < 1e-9 {
                    sweep / count as f64
                } else {
                    sweep / (count.max(2) - 1) as f64
                };
                placements = (0..count)
                    .map(|k| {
                        let t = start + step * k as f64;
                        let r = t.to_radians();
                        ([radius * r.cos(), radius * r.sin(), 0.0], rotate + t)
                    })
                    .collect();
            } else if let Some(l) = p.get("linear") {
                let count = val(&l["count"], &vars, &what("pattern count"))?.round() as usize;
                let dx = l
                    .get("dx")
                    .map(|x| val(x, &vars, &what("pattern dx")))
                    .transpose()?
                    .unwrap_or(0.0);
                let dy = l
                    .get("dy")
                    .map(|y| val(y, &vars, &what("pattern dy")))
                    .transpose()?
                    .unwrap_or(0.0);
                let dz = l
                    .get("dz")
                    .map(|z| val(z, &vars, &what("pattern dz")))
                    .transpose()?
                    .unwrap_or(0.0);
                let rows = l
                    .get("rows")
                    .map(|r| val(r, &vars, &what("pattern rows")))
                    .transpose()?
                    .unwrap_or(1.0)
                    .round()
                    .max(1.0) as usize;
                let row_dy = l
                    .get("row_dy")
                    .map(|y| val(y, &vars, &what("pattern row_dy")))
                    .transpose()?
                    .unwrap_or(0.0);
                placements = (0..rows)
                    .flat_map(|r| {
                        (0..count.max(1)).map(move |k| {
                            (
                                [
                                    dx * k as f64,
                                    dy * k as f64 + row_dy * r as f64,
                                    dz * k as f64,
                                ],
                                rotate,
                            )
                        })
                    })
                    .collect();
            } else {
                return Err(what("pattern must be circular or linear"));
            }
        }
        for (k, (local, r)) in placements.into_iter().enumerate() {
            let t = axis.apply(local);
            out.push(Instance {
                op,
                shape: shape.clone(),
                at: [at[0] + t[0], at[1] + t[1], at[2] + t[2]],
                axis,
                local,
                body: n,
                rotate: r,
                label: if k == 0 {
                    label.clone()
                } else {
                    format!("{label} #{}", k + 1)
                },
            });
        }
        if out.len() > MAX_INSTANCES {
            return Err(format!("more than {MAX_INSTANCES} bodies after patterns"));
        }
    }
    if out.first().is_none_or(|b| b.op != BodyOp::Add) {
        return Err("the first body must be an \"add\" body".into());
    }
    Ok(out)
}

/// Approximate overall size `[w, d, h]` from the bodies that add material.
pub fn extents(insts: &[Instance]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for i in insts.iter().filter(|i| i.op == BodyOp::Add) {
        let (r_xy, zmin, zmax): (Vec<[f64; 2]>, f64, f64) = match &i.shape {
            Shape::Prism { profile, h } => (profile.clone(), 0.0, *h),
            Shape::Cylinder { r, h } => (vec![[-r, -r], [*r, *r]], 0.0, *h),
            Shape::Revolve { section } => {
                let r = section.iter().map(|p| p[0]).fold(0.0, f64::max);
                let (z0, z1) = section
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
                        (a.min(p[1]), b.max(p[1]))
                    });
                (vec![[-r, -r], [r, r]], z0, z1)
            }
        };
        let (s, c) = i.rotate.to_radians().sin_cos();
        let (mut blo, mut bhi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in r_xy {
            let x = p[0] * c - p[1] * s;
            let y = p[0] * s + p[1] * c;
            blo = [blo[0].min(x), blo[1].min(y)];
            bhi = [bhi[0].max(x), bhi[1].max(y)];
        }
        for x in [blo[0], bhi[0]] {
            for y in [blo[1], bhi[1]] {
                for z in [zmin, zmax] {
                    let w = i.axis.apply([x, y, z]);
                    for k in 0..3 {
                        lo[k] = lo[k].min(i.at[k] + w[k]);
                        hi[k] = hi[k].max(i.at[k] + w[k]);
                    }
                }
            }
        }
    }
    (lo, hi)
}

// ── Building ────────────────────────────────────────────────────────────────

fn rotated(p: [f64; 2], deg: f64) -> [f64; 2] {
    let (s, c) = deg.to_radians().sin_cos();
    [p[0] * c - p[1] * s, p[0] * s + p[1] * c]
}

fn fmt(x: f64) -> String {
    let s = format!("{x:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Plan steps that build `part` with its local origin at `origin`, leave the
/// result bound to `$part` on layer [`LAYER`], store the definition on it
/// and restore the current layer to `restore_layer`.
pub fn plan(part: &Part, origin: [f64; 3], restore_layer: &str) -> Result<Vec<Value>, String> {
    let insts = instances(part)?;
    let mut steps = Vec::new();
    // Each solid lands on the current layer; make it AI-PART for the build.
    steps.push(json!({"op":"entities_create","entities":[{"type":"Point","location":[origin[0], origin[1], origin[2]],"layer":LAYER}],"bind":"anchor","note":format!("start {}", part.name)}));
    steps.push(json!({"op":"sysvar","set":{"clayer":LAYER}}));
    steps.push(json!({"op":"entities_delete","handles":"$anchor"}));
    let mut names: Vec<(BodyOp, String)> = Vec::new();
    let mut add_groups: Vec<(usize, Vec<String>)> = Vec::new();
    for (n, inst) in insts.iter().enumerate() {
        let name = format!("b{n}");
        let profile_name = format!("{name}_profile");
        match &inst.shape {
            Shape::Prism { profile, h } => {
                let pts: Vec<[f64; 2]> = profile.iter().map(|p| rotated(*p, inst.rotate)).collect();
                steps.push(json!({"op":"entities_create","entities":[{"type":"LwPolyline","vertices":pts,"closed":true,"layer":LAYER}],"bind":profile_name,"note":inst.label}));
                steps.extend(extrude_steps(&profile_name, &name, *h));
            }
            Shape::Cylinder { r, h } => {
                steps.push(json!({"op":"entities_create","entities":[{"type":"Circle","center":[0.0,0.0],"radius":r,"layer":LAYER}],"bind":profile_name,"note":inst.label}));
                steps.extend(extrude_steps(&profile_name, &name, *h));
            }
            Shape::Revolve { section } => {
                // Drawn as (r, z) in XY, revolved about the Y axis, then
                // turned +90° about X so the axis becomes Z: (x, y, z) →
                // (x, -z, y).
                steps.push(json!({"op":"entities_create","entities":[{"type":"LwPolyline","vertices":section,"closed":true,"layer":LAYER}],"bind":profile_name,"note":inst.label}));
                steps.push(json!({"op":"start","cmd":"REVOLVE"}));
                steps.push(
                    json!({"op":"input","kind":"selection","handles":format!("${profile_name}")}),
                );
                steps.push(json!({"op":"input","kind":"enter"}));
                steps.push(json!({"op":"input","kind":"point","point":[0.0,0.0,0.0]}));
                steps.push(json!({"op":"input","kind":"point","point":[0.0,1.0,0.0]}));
                steps.push(json!({"op":"input","kind":"text","text":"360","bind":name}));
                steps.push(json!({"op":"start","cmd":"3DROTATE"}));
                steps.push(json!({"op":"input","kind":"selection","handles":format!("${name}")}));
                steps.push(json!({"op":"input","kind":"enter"}));
                steps.push(json!({"op":"input","kind":"point","point":[0.0,0.0,0.0]}));
                steps.push(json!({"op":"input","kind":"token","text":"X"}));
                steps.push(json!({"op":"input","kind":"text","text":"90"}));
            }
        }
        let base = if inst.axis == Axis::Z {
            inst.at
        } else {
            // Place within its own frame, then tilt the frame onto its axis
            // about the origin; the move below takes it to its base point.
            if inst.local.iter().any(|x| x.abs() > 1e-12) {
                steps.push(json!({"op":"entities_transform","handles":format!("${name}"),"action":"move","vector":inst.local}));
            }
            let (token, angle) = if inst.axis == Axis::X {
                ("Y", "90")
            } else {
                ("X", "-90")
            };
            steps.push(json!({"op":"start","cmd":"3DROTATE"}));
            steps.push(json!({"op":"input","kind":"selection","handles":format!("${name}")}));
            steps.push(json!({"op":"input","kind":"enter"}));
            steps.push(json!({"op":"input","kind":"point","point":[0.0,0.0,0.0]}));
            steps.push(json!({"op":"input","kind":"token","text":token}));
            steps.push(json!({"op":"input","kind":"text","text":angle,"note":format!("{} along {}", inst.label, token_axis(inst.axis))}));
            let t = inst.axis.apply(inst.local);
            [inst.at[0] - t[0], inst.at[1] - t[1], inst.at[2] - t[2]]
        };
        let v = [
            origin[0] + base[0],
            origin[1] + base[1],
            origin[2] + base[2],
        ];
        if v.iter().any(|x| x.abs() > 1e-12) {
            steps.push(json!({"op":"entities_transform","handles":format!("${name}"),"action":"move","vector":v}));
        }
        if inst.op == BodyOp::Add {
            match add_groups.last_mut() {
                Some((b, g)) if *b == inst.body => g.push(format!("${name}")),
                _ => add_groups.push((inst.body, vec![format!("${name}")])),
            }
        }
        names.push((inst.op, name));
    }
    let group = |op: BodyOp| {
        names
            .iter()
            .filter(|(o, _)| *o == op)
            .map(|(_, n)| format!("${n}"))
            .collect::<Vec<_>>()
    };
    let adds = group(BodyOp::Add);
    let cuts = group(BodyOp::Cut);
    let ints = group(BodyOp::Intersect);
    let mut part_ref = adds[0].clone();
    // Join body by body (each with all its pattern copies): one UNION of
    // many touching pieces is far more likely to fail than a chain of
    // smaller ones.
    let labels: Vec<String> = part.source["bodies"]
        .as_array()
        .map(|b| {
            b.iter()
                .enumerate()
                .map(|(n, x)| {
                    x["name"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or(format!("body {}", n + 1))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut acc: Option<String> = None;
    for (k, (body, group)) in add_groups.iter().enumerate() {
        let mut handles: Vec<String> = acc.iter().cloned().collect();
        handles.extend(group.iter().cloned());
        if handles.len() < 2 {
            acc = handles.pop();
            continue;
        }
        let bind = format!("united{k}");
        steps.push(json!({"op":"start","cmd":"UNION"}));
        steps.push(json!({"op":"input","kind":"selection","handles":handles}));
        steps.push(json!({"op":"input","kind":"enter","bind":bind,"note":format!("join {}", labels.get(*body).map(String::as_str).unwrap_or("the added bodies"))}));
        acc = Some(format!("${bind}"));
    }
    if let Some(a) = acc {
        part_ref = a;
    }
    if !cuts.is_empty() {
        // SUBTRACT keeps the base solid's handle.
        steps.push(json!({"op":"start","cmd":"SUBTRACT"}));
        steps.push(json!({"op":"input","kind":"selection","handles":[part_ref]}));
        steps.push(json!({"op":"input","kind":"enter"}));
        steps.push(json!({"op":"input","kind":"selection","handles":cuts}));
        steps.push(json!({"op":"input","kind":"enter","note":"cut holes and pockets"}));
    }
    if !ints.is_empty() {
        let mut all = vec![part_ref.clone()];
        all.extend(ints);
        steps.push(json!({"op":"start","cmd":"INTERSECT"}));
        steps.push(json!({"op":"input","kind":"selection","handles":all}));
        steps.push(json!({"op":"input","kind":"enter","bind":"intersected","note":"keep the common volume"}));
        part_ref = "$intersected".into();
    }
    // Store the definition (and where it was built) on the solid.
    let mut stored = part.to_json();
    stored["origin"] = json!(origin);
    let text = stored.to_string();
    let chunks: Vec<Value> = text
        .as_bytes()
        .chunks(240)
        .map(|c| json!({"code": 1000, "value": String::from_utf8_lossy(c)}))
        .collect();
    if text.chars().any(|c| !c.is_ascii()) {
        // Byte chunks could split a character; keep it ASCII.
        return Err("the part definition must be plain ASCII".into());
    }
    steps.push(json!({"op":"xdata_set","handles":[part_ref],"app":XDATA_APP,"data":chunks,"note":"remember the parameters"}));
    steps.push(json!({"op":"sysvar","set":{"clayer":restore_layer}}));
    Ok(steps)
}

fn token_axis(a: Axis) -> &'static str {
    match a {
        Axis::X => "X",
        Axis::Y => "Y",
        Axis::Z => "Z",
    }
}

fn extrude_steps(profile: &str, name: &str, h: f64) -> Vec<Value> {
    vec![
        json!({"op":"start","cmd":"EXTRUDE"}),
        json!({"op":"input","kind":"selection","handles":format!("${profile}")}),
        json!({"op":"input","kind":"enter"}),
        json!({"op":"input","kind":"text","text":fmt(h),"bind":name}),
    ]
}

/// Rebuild a stored definition from its extended-data strings.
pub fn from_xdata(strings: &[String]) -> Result<(Part, [f64; 3]), String> {
    let text: String = strings.concat();
    let part = parse_part(&text)?;
    let o = &part.source["origin"];
    let origin = [
        o[0].as_f64().unwrap_or(0.0),
        o[1].as_f64().unwrap_or(0.0),
        o[2].as_f64().unwrap_or(0.0),
    ];
    Ok((part, origin))
}

pub fn system_prompt(units: &str) -> String {
    format!(
        r#"You design parametric mechanical parts for a CAD program. Turn the user's request into ONE JSON part definition and nothing else:

{{"name": "<short name>", "units": "{units}",
  "notes": "<standards used, assumptions, simplifications>",
  "parameters": {{"<name>": <number or expression of earlier parameters>, ...}},
  "bodies": [<body>, ...]}}

A body is {{"name": "...", "op": "add" | "cut" | "intersect", <shape>, "at": [x, y, z], "axis": "z" | "x" | "y", "rotate": <deg about its axis>, "pattern": ...}}.
"axis" (default "z") is the direction the shape's own Z axis points: a cylinder, prism, extrude or revolve with "axis": "x" runs from "at" towards +X, and "y" towards +Y. Its pattern lies in the plane across that axis, centred on "at" (use this for pipes, tees, cross holes and flanges on side branches). The shape's own X turns to -Z for axis "x"; its own Y turns to -Z for axis "y".
Shapes (sizes are numbers or expressions over the parameters, e.g. "d/2 + 1"):
- "box": {{"w": .., "d": .., "h": ..}} — centred on "at" in X and Y, base at z, height up
- "cylinder": {{"d": .., "h": ..}} — axis Z, base centre at "at"
- "prism": {{"sides": n, "across_flats": .. (or "d": circumscribed diameter), "h": ..}} — regular polygon, base at z
- "extrude": {{"points": [[x, y], ...], "h": ..}} — closed XY profile relative to "at", extruded up
- "revolve": {{"points": [[r, z], ...]}} — closed half-section (radius ≥ 0, z) turned a full circle about the Z axis through "at"
Patterns: {{"circular": {{"count": n, "radius": .., "start_deg": .., "sweep_deg": 360}}}} or {{"linear": {{"count": n, "dx": .., "dy": .., "dz": .., "rows": m, "row_dy": ..}}}} ("dz" steps along the body's own axis: stacks of fins or plates).

The first body must be "add". All "add" bodies are joined, then "cut" bodies removed, then "intersect" bodies applied. Z is up; put the part on z = 0. Use real standard sizes (ISO metric threads, DIN/ISO hex sizes, ASME where the user asks) and name every important size as a parameter so it can be edited later. Threads are modelled as plain cylinders at the nominal diameter; mention that in "notes". Expressions support + - * / ^ ( ) sqrt sin cos tan (degrees) min max abs round pi.
If the request is not a part you can build this way, answer {{"message": "<why>", "bodies": []}}."#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOLT: &str = r#"{"name":"M8 hex bolt","units":"mm","parameters":{"d":8,"L":40,"s":13,"k":"0.66*d"},
        "bodies":[{"name":"head","op":"add","prism":{"sides":6,"across_flats":"s","h":"k"}},
                  {"name":"shank","op":"add","cylinder":{"d":"d","h":"L"},"at":[0,0,"k"]}]}"#;

    #[test]
    fn expressions() {
        let mut v = BTreeMap::new();
        v.insert("d".to_string(), 8.0);
        assert_eq!(eval_expr("d/2 + 1", &v).unwrap(), 5.0);
        assert_eq!(eval_expr("-(d-2)^2", &v).unwrap(), -36.0);
        assert_eq!(eval_expr("2^-1", &v).unwrap(), 0.5);
        assert_eq!(eval_expr("2^3^2", &v).unwrap(), 512.0);
        assert!((eval_expr("cos(60)*2", &v).unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(eval_expr("max(1, d, 3)", &v).unwrap(), 8.0);
        assert!((eval_expr("2*pi", &v).unwrap() - std::f64::consts::TAU).abs() < 1e-12);
        assert_eq!(eval_expr("1.5e2", &v).unwrap(), 150.0);
        assert!(eval_expr("q + 1", &v).is_err());
        assert!(eval_expr("d / (d - 8)", &v).is_err());
        assert!(eval_expr("d d", &v).is_err());
    }

    #[test]
    fn parses_a_bolt_with_derived_parameters() {
        let p = parse_part(BOLT).unwrap();
        assert_eq!(p.vars()["k"], 0.66 * 8.0);
        let i = instances(&p).unwrap();
        assert_eq!(i.len(), 2);
        let Shape::Prism { profile, .. } = &i[0].shape else {
            panic!()
        };
        // Across flats 13: the flat sits on the X axis.
        let max_x = profile.iter().map(|q| q[0]).fold(f64::MIN, f64::max);
        assert!((max_x - 6.5).abs() < 1e-9, "{max_x}");
        let (lo, hi) = extents(&i);
        assert!((hi[2] - (40.0 + 5.28)).abs() < 1e-9 && lo[2] == 0.0);
    }

    #[test]
    fn parameters_may_refer_to_later_ones_but_not_in_a_cycle() {
        let p = parse_part(r#"{"parameters":{"a":"z*2","z":3},"bodies":[{"op":"add","cylinder":{"d":"a","h":1}}]}"#).unwrap();
        assert_eq!(p.vars()["a"], 6.0);
        assert!(parse_part(
            r#"{"parameters":{"a":"b","b":"a"},"bodies":[{"op":"add","cylinder":{"d":1,"h":1}}]}"#
        )
        .is_err());
    }

    #[test]
    fn patterns_expand() {
        let p = parse_part(r#"{"parameters":{"n":6,"pcd":50},"bodies":[
            {"op":"add","cylinder":{"d":70,"h":10}},
            {"op":"cut","cylinder":{"d":6,"h":10},"pattern":{"circular":{"count":"n","radius":"pcd/2"}}},
            {"op":"cut","box":{"w":2,"d":2,"h":10},"at":[-30,-30],"pattern":{"linear":{"count":3,"dx":5,"rows":2,"row_dy":5}}}]}"#).unwrap();
        let i = instances(&p).unwrap();
        assert_eq!(i.len(), 1 + 6 + 6);
        assert!((i[2].at[0] - 25.0 * 60f64.to_radians().cos()).abs() < 1e-9);
        assert_eq!(i[12].at, [-20.0, -25.0, 0.0]);
    }

    #[test]
    fn a_linear_pattern_can_stack_along_the_axis() {
        let p = parse_part(
            r#"{"bodies":[{"op":"add","box":{"w":10,"d":10,"h":0.5},"at":[0,0,2],
            "pattern":{"linear":{"count":4,"dz":2}}}]}"#,
        )
        .unwrap();
        let i = instances(&p).unwrap();
        assert_eq!(i[3].at, [0.0, 0.0, 8.0]);
        let (lo, hi) = extents(&i);
        assert_eq!((lo[2], hi[2]), (2.0, 8.5));
        // Along a tilted axis the stack follows it.
        let p = parse_part(
            r#"{"bodies":[{"op":"add","cylinder":{"d":4,"h":1},"axis":"x",
            "pattern":{"linear":{"count":3,"dz":5}}}]}"#,
        )
        .unwrap();
        assert_eq!(instances(&p).unwrap()[2].at, [10.0, 0.0, 0.0]);
    }

    #[test]
    fn rejects_bad_definitions() {
        assert!(parse_part(r#"{"bodies":[]}"#).is_err());
        assert!(
            parse_part(r#"{"bodies":[{"op":"cut","cylinder":{"d":1,"h":1}}]}"#)
                .unwrap_err()
                .contains("first body")
        );
        assert!(
            parse_part(r#"{"bodies":[{"op":"add","cylinder":{"d":-1,"h":1}}]}"#)
                .unwrap_err()
                .contains("positive")
        );
        assert!(parse_part(r#"{"bodies":[{"op":"add","sphere":{"d":1}}]}"#).is_err());
        assert!(parse_part(
            r#"{"parameters":{"2x":1},"bodies":[{"op":"add","cylinder":{"d":1,"h":1}}]}"#
        )
        .is_err());
        assert_eq!(
            parse_part(r#"{"message":"I cannot draw cars","bodies":[]}"#).unwrap_err(),
            "I cannot draw cars"
        );
    }

    #[test]
    fn stored_definition_round_trips_with_new_values() {
        let mut p = parse_part(BOLT).unwrap();
        p.set_param("L", "55").unwrap();
        p.set_param("d", "10").unwrap();
        assert!((p.vars()["k"] - 6.6).abs() < 1e-9, "k follows d");
        assert!(
            p.set_param("d", "-3").is_err(),
            "a negative diameter is refused"
        );
        assert_eq!(p.vars()["d"], 10.0, "and leaves the part unchanged");
        let steps = plan(&p, [100.0, 0.0, 0.0], "0").unwrap();
        let xdata = steps.iter().find(|s| s["op"] == "xdata_set").unwrap();
        let strings: Vec<String> = xdata["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["value"].as_str().unwrap().to_string())
            .collect();
        assert!(strings.iter().all(|s| s.len() <= 240));
        let (back, origin) = from_xdata(&strings).unwrap();
        assert_eq!(origin, [100.0, 0.0, 0.0]);
        assert_eq!(back.vars()["L"], 55.0);
        let k = back.params.iter().find(|p| p.name == "k").unwrap();
        assert_eq!(k.expr, json!("0.66*d"), "derived sizes keep their formula");
        assert_eq!(steps.last().unwrap()["set"]["clayer"], "0");
    }
}
