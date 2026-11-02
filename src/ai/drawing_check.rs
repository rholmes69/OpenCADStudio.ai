//! Drawing checker (AICHECK): rule checks against a drafting standard, plus
//! an optional AI review of what the rules cannot judge.
//!
//! The rules are deterministic and run locally on a neutral snapshot of the
//! drawing ([`CheckInput`]), so they are exact, free and testable. The AI
//! review receives the rule findings and a compact description of the
//! dimensions and text, and may add findings of its own (missing or
//! duplicated dimensions, inconsistent notes, title-block gaps), each tagged
//! as coming from the model.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "Error",
            Severity::Warning => "Warning",
            Severity::Info => "Info",
        }
    }
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" | "critical" | "high" => Severity::Error,
            "info" | "low" | "note" => Severity::Info,
            _ => Severity::Warning,
        }
    }
}

/// What a finding offers to repair automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// Erase the listed objects (duplicates, zero-length objects).
    Erase,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    /// Hex handles of the objects concerned.
    pub handles: Vec<String>,
    pub fix: Option<Fix>,
    /// Raised by the AI review rather than a rule.
    pub from_ai: bool,
}

/// Which standard's rules apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardKind {
    Iso,
    Asme,
    Company,
}

impl StandardKind {
    pub const ALL: [StandardKind; 3] =
        [StandardKind::Iso, StandardKind::Asme, StandardKind::Company];
}

impl std::fmt::Display for StandardKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StandardKind::Iso => "ISO 128 / ISO 3098",
            StandardKind::Asme => "ASME Y14.5 / Y14.2",
            StandardKind::Company => "Company standard (file)",
        })
    }
}

/// The rule settings. ISO and ASME come built in; a company standard is a
/// JSON file with these fields (missing fields take the ISO defaults).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Standard {
    pub name: String,
    /// Allowed text heights in millimetres on the plotted sheet.
    pub text_heights_mm: Vec<f64>,
    /// Model-space text may be any allowed height times one of these.
    pub drawing_scales: Vec<f64>,
    pub allow_layer_zero: bool,
    pub require_bylayer: bool,
    /// A regular expression every layer name must match (empty: no rule).
    pub layer_pattern: String,
    pub required_layers: Vec<String>,
    /// Dimensions must stay associated with their geometry.
    pub require_associative_dimensions: bool,
    /// Extra instructions for the AI review (house rules in plain words).
    pub notes: String,
}

impl Default for Standard {
    fn default() -> Self {
        Self::iso()
    }
}

const COMMON_SCALES: &[f64] = &[
    1.0, 2.0, 2.5, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 2000.0, 2500.0,
    5000.0, 0.5, 0.2, 0.1,
];

impl Standard {
    pub fn iso() -> Self {
        Self {
            name: "ISO 128 / ISO 3098".into(),
            // ISO 3098-1 lettering series.
            text_heights_mm: vec![1.8, 2.5, 3.5, 5.0, 7.0, 10.0, 14.0, 20.0],
            drawing_scales: COMMON_SCALES.to_vec(),
            allow_layer_zero: false,
            require_bylayer: true,
            layer_pattern: String::new(),
            required_layers: Vec::new(),
            require_associative_dimensions: true,
            notes: String::new(),
        }
    }

    pub fn asme() -> Self {
        Self {
            name: "ASME Y14.5 / Y14.2".into(),
            // Y14.2: 0.12 in (3 mm) minimum for general notes and dimensions,
            // 0.24 in (6 mm) for titles and drawing numbers; 0.156/0.188 in
            // are the common intermediate sizes.
            text_heights_mm: vec![3.0, 3.048, 3.5, 3.962, 4.775, 6.0, 6.096, 7.0],
            drawing_scales: COMMON_SCALES.to_vec(),
            allow_layer_zero: false,
            require_bylayer: true,
            layer_pattern: String::new(),
            required_layers: Vec::new(),
            require_associative_dimensions: true,
            notes: "Dimensioning follows ASME Y14.5: each feature dimensioned once, no superfluous dimensions, no dimensions to hidden lines.".into(),
        }
    }

    pub fn for_kind(kind: StandardKind, company: Option<&Standard>) -> Standard {
        match kind {
            StandardKind::Iso => Self::iso(),
            StandardKind::Asme => Self::asme(),
            StandardKind::Company => company.cloned().unwrap_or_else(Self::iso),
        }
    }

    pub fn parse(json: &str) -> Result<Standard, String> {
        let mut s: Standard =
            serde_json::from_str(json).map_err(|e| format!("not a valid standard file: {e}"))?;
        if s.name.trim().is_empty() {
            s.name = "Company standard".into();
        }
        if !s.layer_pattern.is_empty() {
            compile_pattern(&s.layer_pattern)?;
        }
        Ok(s)
    }
}

// ── Layer-name patterns ─────────────────────────────────────────────────────
//
// A small glob-like matcher keeps the crate free of a regex dependency:
// `*` any run, `?` one character, `[ABC]` one of, `#` one digit; everything
// else literal, case-insensitive. A pattern may hold alternatives
// separated by `|`. A leading `^` and trailing `$` are accepted and ignored
// (patterns always match the whole name).

fn compile_pattern(p: &str) -> Result<Vec<String>, String> {
    let alts: Vec<String> = p
        .split('|')
        .map(|a| {
            a.trim()
                .trim_start_matches('^')
                .trim_end_matches('$')
                .to_string()
        })
        .filter(|a| !a.is_empty())
        .collect();
    for a in &alts {
        if a.matches('[').count() != a.matches(']').count() {
            return Err(format!("layer pattern \"{a}\" has an unclosed [ ]"));
        }
    }
    if alts.is_empty() {
        return Err("the layer pattern is empty".into());
    }
    Ok(alts)
}

fn glob(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') => (0..=s.len()).any(|k| glob(&p[1..], &s[k..])),
        Some('?') => !s.is_empty() && glob(&p[1..], &s[1..]),
        Some('#') => s.first().is_some_and(char::is_ascii_digit) && glob(&p[1..], &s[1..]),
        Some('[') => {
            let Some(end) = p.iter().position(|c| *c == ']') else {
                return false;
            };
            let set = &p[1..end];
            s.first()
                .is_some_and(|c| set.iter().any(|x| x.eq_ignore_ascii_case(c)))
                && glob(&p[end + 1..], &s[1..])
        }
        Some(c) => s.first().is_some_and(|x| x.eq_ignore_ascii_case(c)) && glob(&p[1..], &s[1..]),
    }
}

pub fn layer_matches(pattern: &str, name: &str) -> bool {
    let Ok(alts) = compile_pattern(pattern) else {
        return true;
    };
    let s: Vec<char> = name.chars().collect();
    alts.iter()
        .any(|a| glob(&a.chars().collect::<Vec<_>>(), &s))
}

// ── Snapshot of the drawing ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
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
        start: f64,
        end: f64,
    },
    Text {
        value: String,
        height: f64,
    },
    Dimension {
        measurement: f64,
        text: Option<String>,
        associative: bool,
        kind: String,
    },
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub handle: String,
    pub kind: ItemKind,
    pub layer: String,
    pub bylayer_color: bool,
    pub bylayer_linetype: bool,
    /// Owner block record (model space or one layout); objects in different
    /// spaces never overlap.
    pub space: String,
    pub paper_space: bool,
    pub bounds: Option<([f64; 2], [f64; 2])>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CheckInput {
    /// Millimetres per drawing unit (`None` when the drawing is unitless).
    pub mm_per_unit: Option<f64>,
    pub layers: Vec<String>,
    pub items: Vec<Item>,
}

// ── Rules ───────────────────────────────────────────────────────────────────

const MAX_LISTED: usize = 500;

fn finding(rule: &str, severity: Severity, message: String, mut handles: Vec<String>) -> Finding {
    handles.truncate(MAX_LISTED);
    Finding {
        rule: rule.into(),
        severity,
        message,
        handles,
        fix: None,
        from_ai: false,
    }
}

/// The first number in a dimension's override text, if any.
fn number_in(text: &str) -> Option<f64> {
    let mut started = false;
    let mut s = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() || (started && (c == '.' || c == ',')) {
            started = true;
            s.push(if c == ',' { '.' } else { c });
        } else if started {
            break;
        }
    }
    s.trim_end_matches('.').parse().ok()
}

fn height_allowed(h_mm: f64, std: &Standard, paper: bool) -> bool {
    let scales: &[f64] = if paper { &[1.0] } else { &std.drawing_scales };
    std.text_heights_mm.iter().any(|allowed| {
        scales
            .iter()
            .any(|s| (h_mm - allowed * s).abs() <= 0.01 * allowed * s + 1e-6)
    })
}

fn overlap(a: &([f64; 2], [f64; 2]), b: &([f64; 2], [f64; 2])) -> bool {
    a.0[0] < b.1[0] && b.0[0] < a.1[0] && a.0[1] < b.1[1] && b.0[1] < a.1[1]
}

fn geom_key(k: &ItemKind) -> Option<String> {
    let q = |x: f64| (x * 1e6).round() as i64;
    Some(match k {
        ItemKind::Line { a, b } => {
            let (p, r) = if (q(a[0]), q(a[1])) <= (q(b[0]), q(b[1])) {
                (a, b)
            } else {
                (b, a)
            };
            format!("L{},{},{},{}", q(p[0]), q(p[1]), q(r[0]), q(r[1]))
        }
        ItemKind::Circle { c, r } => format!("C{},{},{}", q(c[0]), q(c[1]), q(*r)),
        ItemKind::Arc { c, r, start, end } => {
            format!(
                "A{},{},{},{},{}",
                q(c[0]),
                q(c[1]),
                q(*r),
                q(*start),
                q(*end)
            )
        }
        ItemKind::Text { value, height } => format!("T{value}|{}", q(*height)),
        _ => return None,
    })
}

/// Run every rule of `std` over `input`.
pub fn run_rules(input: &CheckInput, std: &Standard) -> Vec<Finding> {
    let mut out = Vec::new();
    let items = &input.items;

    // Layer 0.
    if !std.allow_layer_zero {
        let on_zero: Vec<String> = items
            .iter()
            .filter(|i| {
                i.layer == "0" && !matches!(i.kind, ItemKind::Other(ref t) if t == "Viewport")
            })
            .map(|i| i.handle.clone())
            .collect();
        if !on_zero.is_empty() {
            out.push(finding(
                "LAYER-0",
                Severity::Warning,
                format!(
                    "{} object(s) are on layer 0; put drawing geometry on named layers",
                    on_zero.len()
                ),
                on_zero,
            ));
        }
    }

    // ByLayer properties.
    if std.require_bylayer {
        let overridden: Vec<String> = items
            .iter()
            .filter(|i| !i.bylayer_color || !i.bylayer_linetype)
            .map(|i| i.handle.clone())
            .collect();
        if !overridden.is_empty() {
            out.push(finding(
                "BYLAYER",
                Severity::Info,
                format!(
                    "{} object(s) override the layer's color or linetype (SETBYLAYER resets them)",
                    overridden.len()
                ),
                overridden,
            ));
        }
    }

    // Layer names.
    if !std.layer_pattern.is_empty() {
        let bad: Vec<&String> = input
            .layers
            .iter()
            .filter(|l| l.as_str() != "0" && !l.eq_ignore_ascii_case("Defpoints"))
            .filter(|l| !layer_matches(&std.layer_pattern, l))
            .collect();
        if !bad.is_empty() {
            let names: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
            let handles = items
                .iter()
                .filter(|i| names.contains(&i.layer))
                .map(|i| i.handle.clone())
                .collect();
            out.push(finding(
                "LAYER-NAME",
                Severity::Warning,
                format!(
                    "{} layer name(s) do not follow \"{}\": {}",
                    names.len(),
                    std.layer_pattern,
                    names
                        .iter()
                        .take(12)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                handles,
            ));
        }
    }
    let missing: Vec<&String> = std
        .required_layers
        .iter()
        .filter(|r| !input.layers.iter().any(|l| l.eq_ignore_ascii_case(r)))
        .collect();
    if !missing.is_empty() {
        out.push(finding(
            "LAYER-MISSING",
            Severity::Warning,
            format!(
                "required layer(s) missing: {}",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Vec::new(),
        ));
    }

    // Dimensions.
    let mut unassoc = Vec::new();
    let mut fake = Vec::new();
    let mut replaced = Vec::new();
    for i in items {
        if let ItemKind::Dimension {
            measurement,
            text,
            associative,
            ..
        } = &i.kind
        {
            if std.require_associative_dimensions && !associative && !i.paper_space {
                unassoc.push(i.handle.clone());
            }
            if let Some(t) = text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                if !t.contains("<>") {
                    match number_in(t) {
                        Some(n)
                            if (n - measurement.abs()).abs()
                                > 0.005 * measurement.abs().max(1e-9) + 1e-9 =>
                        {
                            fake.push(i.handle.clone())
                        }
                        Some(_) => {}
                        None => replaced.push(i.handle.clone()),
                    }
                }
            }
        }
    }
    if !fake.is_empty() {
        out.push(finding(
            "DIM-VALUE",
            Severity::Error,
            format!(
                "{} dimension(s) show a typed value that differs from the measured size",
                fake.len()
            ),
            fake,
        ));
    }
    if !replaced.is_empty() {
        out.push(finding(
            "DIM-TEXT",
            Severity::Warning,
            format!("{} dimension(s) replace the measurement with text (use <> to keep the measured value)", replaced.len()),
            replaced,
        ));
    }
    if !unassoc.is_empty() {
        out.push(finding(
            "DIM-ASSOC",
            Severity::Warning,
            format!("{} dimension(s) are not associated with geometry and will not follow edits (DIMREASSOCIATE)", unassoc.len()),
            unassoc,
        ));
    }

    // Text heights.
    if let Some(mm) = input.mm_per_unit {
        let mut odd = Vec::new();
        let mut sizes: std::collections::BTreeSet<String> = Default::default();
        for i in items {
            if let ItemKind::Text { height, .. } = &i.kind {
                let h_mm = height * mm;
                if *height > 0.0 && !height_allowed(h_mm, std, i.paper_space) {
                    odd.push(i.handle.clone());
                    sizes.insert(
                        format!("{:.4}", height)
                            .trim_end_matches('0')
                            .trim_end_matches('.')
                            .to_string(),
                    );
                }
            }
        }
        if !odd.is_empty() {
            out.push(finding(
                "TEXT-HEIGHT",
                Severity::Warning,
                format!(
                    "{} text object(s) use heights outside the {} series ({}): {}",
                    odd.len(),
                    std.name,
                    std.text_heights_mm
                        .iter()
                        .map(|h| format!("{h}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    sizes.into_iter().take(8).collect::<Vec<_>>().join(", ")
                ),
                odd,
            ));
        }
    }

    // Overlapping annotation (text and dimensions), within one space.
    let mut notes: Vec<&Item> = items
        .iter()
        .filter(|i| {
            matches!(i.kind, ItemKind::Text { .. } | ItemKind::Dimension { .. })
                && i.bounds.is_some()
        })
        .take(5000)
        .collect();
    notes.sort_by(|a, b| a.bounds.unwrap().0[0].total_cmp(&b.bounds.unwrap().0[0]));
    let mut clashes = Vec::new();
    let mut pairs = 0;
    for (n, a) in notes.iter().enumerate() {
        let ab = a.bounds.unwrap();
        for b in &notes[n + 1..] {
            let bb = b.bounds.unwrap();
            if bb.0[0] >= ab.1[0] {
                break;
            }
            // Dimensions overlap their own geometry by design; only
            // text-on-text and text-on-dimension clashes are reported.
            let both_dims = matches!(a.kind, ItemKind::Dimension { .. })
                && matches!(b.kind, ItemKind::Dimension { .. });
            if a.space == b.space && !both_dims && overlap(&ab, &bb) {
                pairs += 1;
                clashes.push(a.handle.clone());
                clashes.push(b.handle.clone());
            }
        }
    }
    clashes.sort();
    clashes.dedup();
    if pairs > 0 {
        out.push(finding(
            "TEXT-OVERLAP",
            Severity::Warning,
            format!("{pairs} pair(s) of text / dimension annotations overlap"),
            clashes,
        ));
    }

    // Duplicates and degenerate geometry.
    let mut seen: std::collections::HashMap<(String, String, String), &Item> = Default::default();
    let mut dups = Vec::new();
    let mut degenerate = Vec::new();
    for i in items {
        match &i.kind {
            ItemKind::Line { a, b } if (a[0] - b[0]).hypot(a[1] - b[1]) < 1e-9 => {
                degenerate.push(i.handle.clone());
                continue;
            }
            ItemKind::Circle { r, .. } | ItemKind::Arc { r, .. } if *r < 1e-9 => {
                degenerate.push(i.handle.clone());
                continue;
            }
            _ => {}
        }
        if let Some(key) = geom_key(&i.kind) {
            if seen
                .insert((i.space.clone(), i.layer.clone(), key), i)
                .is_some()
            {
                dups.push(i.handle.clone());
            }
        }
    }
    if !dups.is_empty() {
        let mut f = finding(
            "DUPLICATE",
            Severity::Warning,
            format!(
                "{} object(s) exactly duplicate another on the same layer",
                dups.len()
            ),
            dups,
        );
        f.fix = Some(Fix::Erase);
        out.push(f);
    }
    if !degenerate.is_empty() {
        let mut f = finding(
            "ZERO-SIZE",
            Severity::Warning,
            format!(
                "{} zero-length line(s) or zero-radius circle(s)/arc(s)",
                degenerate.len()
            ),
            degenerate,
        );
        f.fix = Some(Fix::Erase);
        out.push(f);
    }

    out.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.rule.cmp(&b.rule))
    });
    out
}

// ── AI review ───────────────────────────────────────────────────────────────

/// What the model sees: the standard, the rule findings and a compact list of
/// dimensions, text and geometry counts (capped).
pub fn review_context(input: &CheckInput, std: &Standard, findings: &[Finding]) -> Value {
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut dims = Vec::new();
    let mut texts = Vec::new();
    let mut geometry = Vec::new();
    for i in &input.items {
        match &i.kind {
            ItemKind::Line { a, b } => {
                *counts.entry("line").or_default() += 1;
                if geometry.len() < 300 {
                    geometry.push(json!({"h": i.handle, "line": [a, b], "layer": i.layer}));
                }
            }
            ItemKind::Circle { c, r } => {
                *counts.entry("circle").or_default() += 1;
                if geometry.len() < 300 {
                    geometry.push(json!({"h": i.handle, "circle": [c, r], "layer": i.layer}));
                }
            }
            ItemKind::Arc { c, r, start, end } => {
                *counts.entry("arc").or_default() += 1;
                if geometry.len() < 300 {
                    geometry
                        .push(json!({"h": i.handle, "arc": [c, r, start, end], "layer": i.layer}));
                }
            }
            ItemKind::Text { value, height } => {
                *counts.entry("text").or_default() += 1;
                if texts.len() < 200 {
                    texts.push(json!({"h": i.handle, "text": value.chars().take(120).collect::<String>(), "height": height, "paper": i.paper_space}));
                }
            }
            ItemKind::Dimension {
                measurement,
                text,
                associative,
                kind,
            } => {
                *counts.entry("dimension").or_default() += 1;
                if dims.len() < 300 {
                    dims.push(json!({"h": i.handle, "type": kind, "value": measurement, "text": text, "associative": associative, "bounds": i.bounds}));
                }
            }
            ItemKind::Other(t) => *counts.entry(t.as_str()).or_default() += 1,
        }
    }
    json!({
        "standard": std.name,
        "house_rules": std.notes,
        "mm_per_unit": input.mm_per_unit,
        "layers": input.layers.iter().take(200).collect::<Vec<_>>(),
        "object_counts": counts,
        "dimensions": dims,
        "text": texts,
        "geometry_sample": geometry,
        "rule_findings": findings.iter().map(|f| json!({"rule": f.rule, "severity": f.severity.label(), "message": f.message})).collect::<Vec<_>>(),
    })
}

pub fn review_prompt(context: &Value) -> String {
    format!(
        r#"You are a senior drafting checker reviewing a CAD drawing against {std}. Automatic rules have already run; their findings are listed — do not repeat them. Look for what rules cannot judge:
- features whose size or position is not dimensioned, or is dimensioned twice (over-dimensioning, closed dimension chains);
- dimensions placed on hidden lines, or reading from an inconsistent datum;
- notes, title-block fields or revision text that are missing, inconsistent or contradict dimensions;
- anything the house rules forbid.
Only report what the data supports; reference objects by their "h" handle values.

Answer with ONE JSON object:
{{"summary": "<two sentences>", "findings": [{{"severity": "error"|"warning"|"info", "message": "<what and why>", "handles": ["<h>", ...]}}]}}

Drawing data (JSON):
{ctx}"#,
        std = context["standard"].as_str().unwrap_or("the standard"),
        ctx = serde_json::to_string(context).unwrap_or_default()
    )
}

/// Parse the review; handles are kept only if they exist in `input`.
pub fn parse_review(reply: &str, input: &CheckInput) -> Result<(String, Vec<Finding>), String> {
    let v = super::client::extract_json(reply).ok_or("the model did not answer with JSON")?;
    let known: std::collections::HashSet<&str> =
        input.items.iter().map(|i| i.handle.as_str()).collect();
    let findings = v["findings"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|f| {
                    let message = f["message"].as_str()?.trim().to_string();
                    (!message.is_empty()).then(|| Finding {
                        rule: "AI".into(),
                        severity: Severity::parse(f["severity"].as_str().unwrap_or("")),
                        message,
                        handles: f["handles"]
                            .as_array()
                            .map(|h| {
                                h.iter()
                                    .filter_map(Value::as_str)
                                    .map(|s| s.trim().trim_start_matches("0x").to_ascii_uppercase())
                                    .filter(|s| known.contains(s.as_str()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        fix: None,
                        from_ai: true,
                    })
                })
                .take(50)
                .collect()
        })
        .unwrap_or_default();
    Ok((
        v["summary"].as_str().unwrap_or("").trim().to_string(),
        findings,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(h: &str, kind: ItemKind) -> Item {
        Item {
            handle: h.into(),
            kind,
            layer: "PART".into(),
            bylayer_color: true,
            bylayer_linetype: true,
            space: "MODEL".into(),
            paper_space: false,
            bounds: None,
        }
    }

    fn rules(items: Vec<Item>, std: &Standard) -> Vec<Finding> {
        run_rules(
            &CheckInput {
                mm_per_unit: Some(1.0),
                layers: vec!["0".into(), "PART".into()],
                items,
            },
            std,
        )
    }

    fn has(f: &[Finding], rule: &str) -> Option<Finding> {
        f.iter().find(|x| x.rule == rule).cloned()
    }

    #[test]
    fn a_clean_drawing_has_no_findings() {
        let f = rules(
            vec![
                item(
                    "1",
                    ItemKind::Line {
                        a: [0.0, 0.0],
                        b: [10.0, 0.0],
                    },
                ),
                item(
                    "2",
                    ItemKind::Text {
                        value: "A".into(),
                        height: 3.5,
                    },
                ),
                item(
                    "3",
                    ItemKind::Dimension {
                        measurement: 10.0,
                        text: Some("<> TYP".into()),
                        associative: true,
                        kind: "Linear".into(),
                    },
                ),
            ],
            &Standard::iso(),
        );
        assert!(f.is_empty(), "{f:?}");
    }

    #[test]
    fn flags_layer_zero_overrides_and_odd_text() {
        let mut a = item(
            "A",
            ItemKind::Text {
                value: "x".into(),
                height: 3.0,
            },
        );
        a.layer = "0".into();
        a.bylayer_color = false;
        // 350 = 3.5 at 1:100 — allowed in model space.
        let b = item(
            "B",
            ItemKind::Text {
                value: "y".into(),
                height: 350.0,
            },
        );
        let f = rules(vec![a, b], &Standard::iso());
        assert_eq!(has(&f, "LAYER-0").unwrap().handles, vec!["A"]);
        assert_eq!(has(&f, "BYLAYER").unwrap().handles, vec!["A"]);
        assert_eq!(has(&f, "TEXT-HEIGHT").unwrap().handles, vec!["A"]);
        // ASME allows 3 mm.
        assert!(has(
            &rules(
                vec![item(
                    "C",
                    ItemKind::Text {
                        value: "z".into(),
                        height: 3.0
                    }
                )],
                &Standard::asme()
            ),
            "TEXT-HEIGHT"
        )
        .is_none());
    }

    #[test]
    fn flags_fake_and_replaced_dimension_text() {
        let f = rules(
            vec![
                item(
                    "1",
                    ItemKind::Dimension {
                        measurement: 10.0,
                        text: Some("12".into()),
                        associative: true,
                        kind: "Linear".into(),
                    },
                ),
                item(
                    "2",
                    ItemKind::Dimension {
                        measurement: 10.0,
                        text: Some("10,00".into()),
                        associative: true,
                        kind: "Linear".into(),
                    },
                ),
                item(
                    "3",
                    ItemKind::Dimension {
                        measurement: 10.0,
                        text: Some("SEE NOTE".into()),
                        associative: false,
                        kind: "Linear".into(),
                    },
                ),
            ],
            &Standard::iso(),
        );
        assert_eq!(has(&f, "DIM-VALUE").unwrap().handles, vec!["1"]);
        assert_eq!(has(&f, "DIM-TEXT").unwrap().handles, vec!["3"]);
        assert_eq!(has(&f, "DIM-ASSOC").unwrap().handles, vec!["3"]);
        assert_eq!(f[0].severity, Severity::Error, "errors sort first");
    }

    #[test]
    fn finds_duplicates_zero_size_and_overlaps() {
        let mut t1 = item(
            "T1",
            ItemKind::Text {
                value: "A".into(),
                height: 3.5,
            },
        );
        t1.bounds = Some(([0.0, 0.0], [10.0, 3.5]));
        let mut t2 = item(
            "T2",
            ItemKind::Text {
                value: "B".into(),
                height: 3.5,
            },
        );
        t2.bounds = Some(([5.0, 1.0], [15.0, 4.5]));
        let mut t3 = item(
            "T3",
            ItemKind::Text {
                value: "C".into(),
                height: 3.5,
            },
        );
        t3.bounds = Some(([5.0, 1.0], [15.0, 4.5]));
        t3.space = "LAYOUT1".into();
        let f = rules(
            vec![
                item(
                    "L1",
                    ItemKind::Line {
                        a: [0.0, 0.0],
                        b: [5.0, 5.0],
                    },
                ),
                item(
                    "L2",
                    ItemKind::Line {
                        a: [5.0, 5.0],
                        b: [0.0, 0.0],
                    },
                ),
                item(
                    "Z",
                    ItemKind::Line {
                        a: [1.0, 1.0],
                        b: [1.0, 1.0],
                    },
                ),
                t1,
                t2,
                t3,
            ],
            &Standard::iso(),
        );
        let d = has(&f, "DUPLICATE").unwrap();
        assert_eq!(d.handles, vec!["L2"]);
        assert_eq!(d.fix, Some(Fix::Erase));
        assert_eq!(has(&f, "ZERO-SIZE").unwrap().handles, vec!["Z"]);
        assert_eq!(has(&f, "TEXT-OVERLAP").unwrap().handles, vec!["T1", "T2"]);
    }

    #[test]
    fn layer_patterns_and_company_files() {
        assert!(layer_matches("^[ASMEC]-*$", "A-WALL"));
        assert!(!layer_matches("[ASMEC]-*", "WALLS"));
        assert!(layer_matches("DIM|TEXT|P-##", "p-07"));
        let s = Standard::parse(r#"{"name":"ACME","layer_pattern":"[ASMEC]-*","required_layers":["A-TITLE"],"allow_layer_zero":true}"#).unwrap();
        let f = rules(
            vec![item(
                "1",
                ItemKind::Line {
                    a: [0.0, 0.0],
                    b: [1.0, 0.0],
                },
            )],
            &s,
        );
        assert!(has(&f, "LAYER-NAME").unwrap().message.contains("PART"));
        assert!(has(&f, "LAYER-MISSING")
            .unwrap()
            .message
            .contains("A-TITLE"));
        assert_eq!(
            s.text_heights_mm,
            Standard::iso().text_heights_mm,
            "missing fields default to ISO"
        );
        assert!(Standard::parse(r#"{"layer_pattern":"[A-"}"#).is_err());
    }

    #[test]
    fn review_keeps_only_known_handles() {
        let input = CheckInput {
            mm_per_unit: Some(1.0),
            layers: vec![],
            items: vec![item(
                "2A",
                ItemKind::Line {
                    a: [0.0, 0.0],
                    b: [1.0, 0.0],
                },
            )],
        };
        let (summary, f) = parse_review(
            r#"{"summary":"ok","findings":[{"severity":"error","message":"hole not located","handles":["2a","FFFF"]},{"message":""}]}"#,
            &input,
        )
        .unwrap();
        assert_eq!(summary, "ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].handles, vec!["2A"]);
        assert!(f[0].from_ai);
        let ctx = review_context(&input, &Standard::iso(), &[]);
        assert!(review_prompt(&ctx).contains("ISO 128"));
    }
}
