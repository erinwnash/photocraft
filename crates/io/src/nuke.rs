//! Nuke `.nk` Roto exchange: reads the shapes of a `Roto` / `RotoPaint` node from script text (a
//! file, or what Nuke puts on the clipboard when you copy the node) and writes a `Roto` node that
//! pastes straight into Nuke's node graph.
//!
//! Only the structure confirmed against real Roto nodes is mapped (see
//! `docs/superpowers/specs/2026-10-06-roto-mask-design.md` §6.1):
//!
//! * Floats are `x` + up to 8 hex digits (an `f32`'s bits), or decimal; a bare `0` is zero.
//! * `{layer NAME …}` is a group (the one called `Root` is the mask itself); `{curvegroup NAME
//!   FLAGS bezier …}` is a shape.
//! * A shape holds two `cc` curves, the outline and the feather outline. In each `{px 1 …}` list
//!   a point is three `{x y}` entries: `[tangent_in, position, tangent_out]`, tangents relative to
//!   the position. In the feather curve the position slot is the feather handle's offset from the
//!   shape point and the tangents are absolute, so they become offsets from the shape's own.
//! * Nuke's y axis points up; document space points down, so y is flipped by the format height.
//! * `{tx 1 X Y}` is the shape's transform pivot (it equals the centroid of the points), not a
//!   translation, and is ignored on import.
//!
//! Anything else (animation, strokes, other shape kinds, opacity and blend attributes whose names
//! are not yet confirmed) is skipped and reported in the returned warnings, never an error. The
//! text is untrusted: size, token count and nesting are capped and nothing panics.

use std::collections::HashSet;

use photocraft_doc::RotoMask;
use photocraft_doc::roto::{Group, MAX_POINTS_PER_SHAPE, Node, NodeId, Point, PointId, Shape, V2};

/// Largest script accepted.
pub const MAX_NK_BYTES: usize = 32 << 20;
const MAX_TOKENS: usize = 2_000_000;
const MAX_DEPTH: usize = 64;
const MAX_WARNINGS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NukeError {
    #[error("script is larger than {} MiB", MAX_NK_BYTES >> 20)]
    TooLarge,
    #[error("{0}")]
    Syntax(String),
    #[error("no Roto or RotoPaint node found")]
    NoRoto,
}

fn syntax(msg: impl Into<String>) -> NukeError {
    NukeError::Syntax(msg.into())
}

// ---------------------------------------------------------------------------
// Tokenizer: Nuke text to a tree of tokens and braces
// ---------------------------------------------------------------------------

enum Item<'a> {
    Tok(&'a str),
    Group(Vec<Item<'a>>),
}

fn tokenize(text: &str) -> Result<Vec<Item<'_>>, NukeError> {
    let b = text.as_bytes();
    let mut stack: Vec<Vec<Item>> = Vec::new();
    let mut cur: Vec<Item> = Vec::new();
    let (mut i, mut count) = (0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'{' => {
                if stack.len() >= MAX_DEPTH {
                    return Err(syntax("braces nested too deeply"));
                }
                stack.push(std::mem::take(&mut cur));
                i += 1;
            }
            b'}' => {
                let parent = stack.pop().ok_or_else(|| syntax("unbalanced `}`"))?;
                let group = std::mem::replace(&mut cur, parent);
                cur.push(Item::Group(group));
                i += 1;
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if j >= b.len() {
                    return Err(syntax("unterminated string"));
                }
                count += 1;
                if count > MAX_TOKENS {
                    return Err(syntax("script has too many tokens"));
                }
                cur.push(Item::Tok(text.get(start..j).unwrap_or("")));
                i = j + 1;
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'{' && b[i] != b'}' {
                    i += 1;
                }
                count += 1;
                if count > MAX_TOKENS {
                    return Err(syntax("script has too many tokens"));
                }
                cur.push(Item::Tok(text.get(start..i).unwrap_or("")));
            }
        }
    }
    if stack.is_empty() { Ok(cur) } else { Err(syntax("unclosed `{`")) }
}

fn first_tok<'a>(items: &[Item<'a>]) -> Option<&'a str> {
    match items.first() {
        Some(Item::Tok(t)) => Some(t),
        _ => None,
    }
}

fn tok<'a>(items: &[Item<'a>], i: usize) -> Option<&'a str> {
    match items.get(i) {
        Some(Item::Tok(t)) => Some(t),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

struct Ctx {
    height: f64,
    next_node: u64,
    next_point: u64,
    warnings: Vec<String>,
    seen: HashSet<String>,
}

impl Ctx {
    fn warn(&mut self, msg: String) {
        if self.warnings.len() < MAX_WARNINGS && self.seen.insert(msg.clone()) {
            self.warnings.push(msg);
        }
    }

    fn node_id(&mut self) -> NodeId {
        self.next_node += 1;
        NodeId(self.next_node)
    }

    fn point_id(&mut self) -> PointId {
        self.next_point += 1;
        PointId(self.next_point)
    }

    /// A Nuke float token; unparsable or non-finite values become zero with a warning.
    fn num(&mut self, t: &str) -> f64 {
        if t == "0" {
            return 0.0;
        }
        let parsed = match t.strip_prefix('x') {
            Some(hex) if !hex.is_empty() && hex.len() <= 8 && hex.bytes().all(|c| c.is_ascii_hexdigit()) => {
                u32::from_str_radix(hex, 16).ok().map(f32::from_bits)
            }
            _ => t.parse::<f32>().ok(),
        };
        match parsed {
            Some(v) if v.is_finite() => f64::from(v),
            _ => {
                self.warn(format!("number `{}` is not a finite value; used 0", t.chars().take(24).collect::<String>()));
                0.0
            }
        }
    }
}

/// Attribute names in a shape's `{a …}` block that carry only editor state, with no mask meaning.
const IGNORED_ATTRS: [&str; 7] = ["osw", "osf", "str", "spx", "spy", "sb", "tt"];
/// Element names that are structure or editor state rather than shapes.
const KNOWN_ELEMENTS: [&str; 8] = ["f", "t", "a", "v", "n", "cc", "px", "tx"];

type Triple = [(f64, f64); 3];

/// The `[tangent_in, position, tangent_out]` triples of a `cc` curve, or `None` (with a warning)
/// when the points are animated or otherwise not plain `{x y}` pairs.
fn triples(cc: &[Item], name: &str, cx: &mut Ctx) -> Result<Option<Vec<Triple>>, NukeError> {
    let Some(px) = cc.iter().find_map(|it| match it {
        Item::Group(g) if first_tok(g) == Some("px") => Some(g),
        _ => None,
    }) else {
        return Ok(Some(Vec::new()));
    };
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    for it in px {
        let Item::Group(g) = it else { continue };
        let [Item::Tok(x), Item::Tok(y)] = g.as_slice() else {
            cx.warn(format!("shape `{name}`: animated or unsupported point data skipped"));
            return Ok(None);
        };
        if pairs.len() >= MAX_POINTS_PER_SHAPE * 3 {
            return Err(syntax(format!("shape `{name}` has too many points")));
        }
        pairs.push((cx.num(x), cx.num(y)));
    }
    if !pairs.len().is_multiple_of(3) {
        return Err(syntax(format!("shape `{name}`: point data is not a whole number of [tangent, position, tangent] triples")));
    }
    Ok(Some(pairs.as_chunks::<3>().0.to_vec()))
}

fn is_smooth(tin: V2, tout: V2) -> bool {
    let (a, b) = (tin.x.hypot(tin.y), tout.x.hypot(tout.y));
    a > 1e-9 && b > 1e-9 && (tin.x * tout.y - tin.y * tout.x).abs() <= 1e-3 * a * b && tin.x * tout.x + tin.y * tout.y < 0.0
}

fn read_shape(g: &[Item], out: &mut Vec<Node>, cx: &mut Ctx) -> Result<(), NukeError> {
    let name = tok(g, 1).unwrap_or("Shape");
    let kind = tok(g, 3).unwrap_or("");
    if kind != "bezier" {
        cx.warn(format!("shape `{name}`: unsupported kind `{kind}` skipped"));
        return Ok(());
    }
    let Some(curves) = g.iter().find_map(|it| match it {
        Item::Group(c) if matches!(c.first(), Some(Item::Group(f)) if first_tok(f) == Some("cc")) => Some(c),
        _ => None,
    }) else {
        cx.warn(format!("shape `{name}` has no curve data"));
        return Ok(());
    };
    let ccs: Vec<&[Item]> = curves
        .iter()
        .filter_map(|it| match it {
            Item::Group(c) if first_tok(c) == Some("cc") => Some(c.as_slice()),
            _ => None,
        })
        .collect();
    let Some(outline) = ccs.first().map(|cc| triples(cc, name, cx)).transpose()?.flatten() else { return Ok(()) };
    let feather = match ccs.get(1) {
        Some(cc) => triples(cc, name, cx)?.unwrap_or_default(),
        None => Vec::new(),
    };
    if !feather.is_empty() && feather.len() != outline.len() {
        cx.warn(format!("shape `{name}`: feather curve has {} points for {} shape points; feather ignored", feather.len(), outline.len()));
    }
    let use_feather = feather.len() == outline.len();
    for it in g {
        let Item::Group(a) = it else { continue };
        if first_tok(a) != Some("a") {
            continue;
        }
        for pair in a.get(1..).unwrap_or_default().chunks(2) {
            if let Some(Item::Tok(key)) = pair.first()
                && !IGNORED_ATTRS.contains(key)
            {
                cx.warn(format!("shape attribute `{key}` is not imported"));
            }
        }
    }
    let h = cx.height;
    let mut shape = Shape::new(cx.node_id(), name);
    for (k, [tin, pos, tout]) in outline.iter().enumerate() {
        let mut p = Point::corner(cx.point_id(), pos.0 + 0.0, h - pos.1 + 0.0);
        p.tangent_in = V2::new(tin.0 + 0.0, 0.0 - tin.1);
        p.tangent_out = V2::new(tout.0 + 0.0, 0.0 - tout.1);
        p.smooth = is_smooth(p.tangent_in, p.tangent_out);
        if use_feather && let Some([ftin, fpos, ftout]) = feather.get(k) {
            p.feather_pos = V2::new(fpos.0 + 0.0, 0.0 - fpos.1);
            // The file's feather tangents are absolute; ours are relative to the shape's.
            p.feather_in = V2::new(ftin.0 - tin.0 + 0.0, tin.1 - ftin.1 + 0.0);
            p.feather_out = V2::new(ftout.0 - tout.0 + 0.0, tout.1 - ftout.1 + 0.0);
        }
        shape.points.push(p);
    }
    out.push(Node::Shape(shape));
    Ok(())
}

fn collect(items: &[Item], out: &mut Vec<Node>, cx: &mut Ctx, top: bool) -> Result<(), NukeError> {
    for it in items {
        let Item::Group(g) = it else { continue };
        match first_tok(g) {
            Some("layer") => {
                let name = tok(g, 1).unwrap_or("Layer");
                let mut kids = Vec::new();
                collect(g.get(2..).unwrap_or_default(), &mut kids, cx, false)?;
                if top && name == "Root" {
                    out.extend(kids);
                } else {
                    let mut group = Group::new(cx.node_id(), name);
                    group.children = kids;
                    out.push(Node::Group(group));
                }
            }
            Some("curvegroup") => read_shape(g, out, cx)?,
            Some(other) => {
                if !KNOWN_ELEMENTS.contains(&other) {
                    cx.warn(format!("element `{other}` is not imported"));
                }
                collect(g, out, cx, top)?;
            }
            None => collect(g, out, cx, top)?,
        }
    }
    Ok(())
}

fn find_roto<'a, 'b>(items: &'b [Item<'a>], out: &mut Vec<&'b [Item<'a>]>) {
    for pair in items.windows(2) {
        if let (Item::Tok(t), Item::Group(body)) = (&pair[0], &pair[1])
            && (*t == "Roto" || *t == "RotoPaint")
        {
            out.push(body);
        }
    }
    for it in items {
        if let Item::Group(g) = it {
            find_roto(g, out);
        }
    }
}

/// Reads the shapes of the first `Roto` / `RotoPaint` node in `text`. `doc_height` is the format
/// height in pixels (Nuke's y axis points up). Returns the mask and a list of what was skipped.
pub fn parse_nk(text: &str, doc_height: f64) -> Result<(RotoMask, Vec<String>), NukeError> {
    if text.len() > MAX_NK_BYTES {
        return Err(NukeError::TooLarge);
    }
    if !(doc_height.is_finite() && doc_height > 0.0) {
        return Err(syntax("the format height must be a positive, finite number"));
    }
    let items = tokenize(text)?;
    let mut nodes: Vec<&[Item]> = Vec::new();
    find_roto(&items, &mut nodes);
    let body = nodes.first().ok_or(NukeError::NoRoto)?;
    let mut cx = Ctx { height: doc_height, next_node: 0, next_point: 0, warnings: Vec::new(), seen: HashSet::new() };
    if nodes.len() > 1 {
        cx.warn(format!("{} further Roto nodes were ignored; only the first was imported", nodes.len() - 1));
    }
    let curves = body.iter().enumerate().find_map(|(i, it)| match (it, body.get(i + 1)) {
        (Item::Tok("curves"), Some(Item::Group(g))) => Some(g.as_slice()),
        _ => None,
    });
    let mut children = Vec::new();
    match curves {
        Some(c) => collect(c, &mut children, &mut cx, true)?,
        None => cx.warn("the Roto node has no curves".into()),
    }
    let mut mask = RotoMask::default();
    mask.root.children = children;
    mask.validate().map_err(|e| syntax(format!("imported data is not valid: {e}")))?;
    Ok((mask, cx.warnings))
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// A Nuke float: `0` for zero, otherwise `x` + the `f32`'s bits in hex.
fn hx(v: f64) -> String {
    let f = v as f32;
    if f == 0.0 || !f.is_finite() { "0".into() } else { format!("x{:08x}", f.to_bits()) }
}

fn pair(x: f64, y: f64) -> String {
    format!("{{{} {}}}", hx(x), hx(y))
}

/// Nuke names are identifiers.
fn ident(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if s.is_empty() { "Shape".into() } else { s }
}

/// Defaults Nuke writes for a layer's `{a …}` block (copied from a real node).
const LAYER_ATTRS: &str = "pt1x 0 pt1y 0 pt2x 0 pt2y 0 pt3x 0 pt3y 0 pt4x 0 pt4y 0 ptex00 0 ptex01 0 ptex02 0 ptex03 0 ptex10 0 ptex11 0 ptex12 0 ptex13 0 ptex20 0 ptex21 0 ptex22 0 ptex23 0 ptex30 0 ptex31 0 ptex32 0 ptex33 0 ptof1x 0 ptof1y 0 ptof2x 0 ptof2y 0 ptof3x 0 ptof3y 0 ptof4x 0 ptof4y 0 pterr 0 ptrefset 0 ptmot x40800000 ptref 0";

fn write_shape(s: &Shape, w: f64, h: f64, out: &mut String) {
    let flip = |v: V2| (v.x, -v.y);
    let mut outline = String::new();
    let mut feather = String::new();
    let (mut cx, mut cy) = (0.0, 0.0);
    for p in &s.points {
        let (tin, tout) = (flip(p.tangent_in), flip(p.tangent_out));
        let pos = (p.pos.x, h - p.pos.y);
        cx += pos.0;
        cy += pos.1;
        outline.push_str(&format!("\n        {}\n        {}\n        {}", pair(tin.0, tin.1), pair(pos.0, pos.1), pair(tout.0, tout.1)));
        // Feather tangents are absolute in the file, relative to the shape's here.
        let ftin = flip(V2::new(p.tangent_in.x + p.feather_in.x, p.tangent_in.y + p.feather_in.y));
        let ftout = flip(V2::new(p.tangent_out.x + p.feather_out.x, p.tangent_out.y + p.feather_out.y));
        let off = flip(p.feather_pos);
        feather.push_str(&format!("\n        {}\n        {}\n        {}", pair(ftin.0, ftin.1), pair(off.0, off.1), pair(ftout.0, ftout.1)));
    }
    let n = s.points.len().max(1) as f64;
    out.push_str(&format!(
        "    {{curvegroup {name} 512 bezier\n     {{{{cc\n       {{f 8192}}\n       {{px 1{outline}}}}}\n      {{cc\n       {{f 8192}}\n       {{px 1{feather}}}}}}}\n     {{tx 1 {tx} {ty}}}\n     {{a osw x41200000 osf 0 str 1 spx {sx} spy {sy} sb 1 tt x40800000}}}}\n",
        name = ident(&s.name),
        tx = hx(cx / n),
        ty = hx(cy / n),
        sx = hx(w / 2.0),
        sy = hx(h / 2.0),
    ));
}

fn write_layer(name: &str, children: &[Node], w: f64, h: f64, out: &mut String) {
    out.push_str(&format!("    {{layer {}\n    {{f 2097152}}\n    {{t {} {}}}\n    {{a {LAYER_ATTRS}}}\n", ident(name), hx(w / 2.0), hx(h / 2.0)));
    for n in children {
        match n {
            Node::Shape(s) => write_shape(s, w, h, out),
            Node::Group(g) => write_layer(&g.name, &g.children, w, h, out),
        }
    }
    out.push_str("    }\n");
}

/// A `.nk` fragment holding one `Roto` node with the mask's shapes (static values only), ready
/// to save or paste into Nuke. `doc_width` and `doc_height` are the format size in pixels.
pub fn write_nk(m: &RotoMask, doc_width: f64, doc_height: f64) -> String {
    let (w, h) = (if doc_width.is_finite() { doc_width } else { 0.0 }, if doc_height.is_finite() { doc_height } else { 0.0 });
    let mut s = String::from(
        "set cut_paste_input [stack 0]\nversion 15.0 v4\npush $cut_paste_input\nRoto {\n output alpha\n cliptype none\n curves {{{v x3f99999a}\n  {f 0}\n  {n\n",
    );
    write_layer("Root", &m.root.children, w, h, &mut s);
    s.push_str("  }}}\n name Roto1\n}\n");
    s
}
