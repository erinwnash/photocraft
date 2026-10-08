//! Roto mask commands (`layer.roto.*`, `roto.*`): a tree of bezier shapes and groups that makes a
//! layer's alpha mask, with per-point and global feather, per-shape opacity and blend ops, group
//! transforms, shape tools, Nuke import/export and baking to a pixel mask.
//!
//! Coordinates are document pixels (y down). Opacities and densities are `0..1` (not percent).
//! Handles (`in`, `out`) and feather offsets are relative to their point; `featherIn` and
//! `featherOut` are relative to the shape's own `in`/`out` tangents. A point is `[x, y]` (a corner)
//! or `{"pos":[x,y],"in":[dx,dy],"out":[dx,dy],"feather":[dx,dy],"featherIn":[dx,dy],
//! "featherOut":[dx,dy],"smooth":bool}`.
//!
//! Every edit is one undoable step and is validated afterwards (finite, bounded, depth and size
//! capped); a rejected edit leaves the document untouched. Stale node or point ids are errors.

use photocraft_doc::roto::{self, BlendOp, Falloff, Group, MAX_BLUR, Node, NodeId, OverlapMode, Point, PointId, RotoMask, Shape, V2};
use photocraft_doc::{LayerId, RotoError};
use photocraft_io::nuke;
use photocraft_vector as vector;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn rerr(cmd: &str, e: RotoError) -> EngineError {
    bad(cmd, e.to_string())
}

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    d.active_layer.filter(|id| d.doc.layer(*id).is_some()).map(|_| ()).ok_or_else(|| "no active layer".into())
}

fn has_selection_and_layer(s: &Session) -> std::result::Result<(), String> {
    has_layer(s)?;
    s.active().filter(|d| d.doc.selection.is_some()).map(|_| ()).ok_or_else(|| "no selection".into())
}

fn has_roto(s: &Session) -> std::result::Result<(), String> {
    has_layer(s)?;
    let d = s.active().ok_or("no document open")?;
    let id = d.active_layer.ok_or("no active layer")?;
    d.doc.layer(id).and_then(|l| l.roto_mask.as_ref()).map(|_| ()).ok_or_else(|| "active layer has no roto mask".into())
}

fn layer_id(s: &Session, p: &Value) -> Result<LayerId> {
    match p.get("layer").and_then(Value::as_u64) {
        Some(id) => Ok(LayerId(id)),
        None => s.active().and_then(|d| d.active_layer).ok_or(EngineError::Other("no active layer".into())),
    }
}

// ---------------------------------------------------------------------------
// Parameter parsing
// ---------------------------------------------------------------------------

fn v2(v: &Value) -> Option<V2> {
    let (x, y) = match v {
        Value::Array(a) if a.len() == 2 => (a[0].as_f64()?, a[1].as_f64()?),
        Value::Object(_) => (v.get("x")?.as_f64()?, v.get("y")?.as_f64()?),
        _ => return None,
    };
    (x.is_finite() && y.is_finite()).then_some(V2::new(x, y))
}

fn req_v2(cmd: &str, p: &Value, key: &str) -> Result<V2> {
    p.get(key).and_then(v2).ok_or_else(|| bad(cmd, format!("`{key}` must be [x, y]")))
}

fn opt_v2(cmd: &str, p: &Value, key: &str) -> Result<Option<V2>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v2(v).map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be [x, y]"))),
    }
}

fn opt_f64(cmd: &str, p: &Value, key: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() => Ok(Some(x)),
            _ => Err(bad(cmd, format!("`{key}` must be a finite number"))),
        },
    }
}

fn opt_bool(cmd: &str, p: &Value, key: &str) -> Result<Option<bool>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be true or false"))),
    }
}

fn unit(cmd: &str, p: &Value, key: &str) -> Result<Option<f32>> {
    match opt_f64(cmd, p, key)? {
        None => Ok(None),
        Some(x) if (0.0..=1.0).contains(&x) => Ok(Some(x as f32)),
        Some(_) => Err(bad(cmd, format!("`{key}` must be between 0 and 1"))),
    }
}

fn node_id(cmd: &str, p: &Value, key: &str) -> Result<NodeId> {
    p.get(key).and_then(Value::as_u64).map(NodeId).ok_or_else(|| bad(cmd, format!("`{key}` must be a node id")))
}

fn node_ids(cmd: &str, p: &Value, key: &str) -> Result<Vec<NodeId>> {
    let a = p.get(key).and_then(Value::as_array).ok_or_else(|| bad(cmd, format!("`{key}` must be an array of node ids")))?;
    a.iter().map(|v| v.as_u64().map(NodeId).ok_or_else(|| bad(cmd, format!("`{key}` must be an array of node ids")))).collect()
}

fn point_ids(cmd: &str, p: &Value, key: &str) -> Result<Option<Vec<PointId>>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_u64().map(PointId).ok_or_else(|| bad(cmd, format!("`{key}` must be an array of point ids"))))
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => Err(bad(cmd, format!("`{key}` must be an array of point ids"))),
    }
}

fn blend_op(s: &str) -> Option<BlendOp> {
    Some(match s {
        "union" => BlendOp::Union,
        "subtract" => BlendOp::Subtract,
        "intersect" => BlendOp::Intersect,
        "max" => BlendOp::Max,
        "min" => BlendOp::Min,
        "multiply" => BlendOp::Multiply,
        "difference" => BlendOp::Difference,
        _ => return None,
    })
}

fn blend_name(b: BlendOp) -> &'static str {
    match b {
        BlendOp::Union => "union",
        BlendOp::Subtract => "subtract",
        BlendOp::Intersect => "intersect",
        BlendOp::Max => "max",
        BlendOp::Min => "min",
        BlendOp::Multiply => "multiply",
        BlendOp::Difference => "difference",
    }
}

fn falloff(s: &str) -> Option<Falloff> {
    Some(match s {
        "linear" => Falloff::Linear,
        "smooth" => Falloff::Smooth,
        "easeIn" => Falloff::EaseIn,
        "easeOut" => Falloff::EaseOut,
        _ => return None,
    })
}

fn falloff_name(f: Falloff) -> &'static str {
    match f {
        Falloff::Linear => "linear",
        Falloff::Smooth => "smooth",
        Falloff::EaseIn => "easeIn",
        Falloff::EaseOut => "easeOut",
    }
}

fn overlap_name(o: OverlapMode) -> &'static str {
    match o {
        OverlapMode::Max => "max",
        OverlapMode::Sum => "sum",
        OverlapMode::Over => "over",
    }
}

fn backend_name(b: roto::Backend) -> &'static str {
    match b {
        roto::Backend::Auto => "auto",
        roto::Backend::Cpu => "cpu",
        roto::Backend::Gpu => "gpu",
    }
}

fn parse_point(cmd: &str, v: &Value, id: PointId) -> Result<Point> {
    if let Some(pos) = v2(v) {
        return Ok(Point::corner(id, pos.x, pos.y));
    }
    let Value::Object(_) = v else { return Err(bad(cmd, "a point must be [x, y] or an object")) };
    let pos = req_v2(cmd, v, "pos")?;
    let mut p = Point::corner(id, pos.x, pos.y);
    if let Some(t) = opt_v2(cmd, v, "in")? {
        p.tangent_in = t;
    }
    if let Some(t) = opt_v2(cmd, v, "out")? {
        p.tangent_out = t;
    }
    if let Some(t) = opt_v2(cmd, v, "feather")? {
        p.feather_pos = t;
    }
    if let Some(t) = opt_v2(cmd, v, "featherIn")? {
        p.feather_in = t;
    }
    if let Some(t) = opt_v2(cmd, v, "featherOut")? {
        p.feather_out = t;
    }
    p.smooth = opt_bool(cmd, v, "smooth")?.unwrap_or(false);
    Ok(p)
}

// ---------------------------------------------------------------------------
// JSON out
// ---------------------------------------------------------------------------

fn j2(v: V2) -> Value {
    json!([v.x, v.y])
}

fn transform_json(t: &roto::Transform2D) -> Value {
    json!({ "translate": j2(t.translate), "rotate": t.rotate, "scale": j2(t.scale), "skew": t.skew, "pivot": j2(t.pivot) })
}

fn point_json(p: &Point) -> Value {
    json!({
        "id": p.id.0, "pos": j2(p.pos), "in": j2(p.tangent_in), "out": j2(p.tangent_out),
        "feather": j2(p.feather_pos), "featherIn": j2(p.feather_in), "featherOut": j2(p.feather_out), "smooth": p.smooth,
    })
}

fn node_json(n: &Node) -> Value {
    match n {
        Node::Shape(s) => json!({
            "kind": "shape", "id": s.id.0, "name": s.name, "visible": s.visible, "locked": s.locked, "opacity": s.opacity,
            "blendOp": blend_name(s.blend_op), "invert": s.invert, "closed": s.closed, "blur": s.blur,
            "color": s.color,
            "falloff": falloff_name(s.falloff), "transform": transform_json(&s.transform),
            "points": s.points.iter().map(point_json).collect::<Vec<_>>(),
        }),
        Node::Group(g) => json!({
            "kind": "group", "id": g.id.0, "name": g.name, "visible": g.visible, "locked": g.locked, "opacity": g.opacity,
            "blendOp": blend_name(g.blend_op), "transform": transform_json(&g.transform),
            "children": g.children.iter().map(node_json).collect::<Vec<_>>(),
        }),
    }
}

fn info_json(layer: LayerId, m: &RotoMask) -> Value {
    json!({
        "layer": layer.0, "enabled": m.enabled, "linked": m.linked, "density": m.density, "invert": m.invert,
        "overlap": overlap_name(m.overlap), "backend": backend_name(m.backend),
        "transform": transform_json(&m.root.transform),
        "nodes": m.root.children.iter().map(node_json).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// Editing scaffold
// ---------------------------------------------------------------------------

/// Runs `f` on the layer's roto mask as one undoable step. The mask is validated afterwards; any
/// error (including a failed validation) leaves the document untouched.
fn with_roto(s: &mut Session, p: &Value, cmd: &'static str, label: &str, f: impl FnOnce(&mut RotoMask) -> Result<Value>) -> Result<Value> {
    let id = layer_id(s, p)?;
    s.edit(label, |doc, _| {
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if l.locks.all {
            return Err(EngineError::Other(format!("layer \"{}\" is locked", l.name)));
        }
        let m = l.roto_mask.as_mut().ok_or_else(|| EngineError::Other("layer has no roto mask (use layer.roto.add)".into()))?;
        let out = f(m)?;
        m.validate().map_err(|e| rerr(cmd, e))?;
        Ok(out)
    })
}

fn shape_of<'a>(cmd: &str, m: &'a mut RotoMask, id: NodeId) -> Result<&'a mut Shape> {
    m.shape_mut(id).ok_or_else(|| bad(cmd, format!("no shape with id {}", id.0)))
}

/// Adds a shape built from `points` (ids are assigned here) under `parent` (default: the root).
fn add_shape_node(cmd: &str, m: &mut RotoMask, p: &Value, default_name: &str, closed: bool, mut points: Vec<Point>) -> Result<Value> {
    let parent = match p.get("parent") {
        None | Some(Value::Null) => m.root.id,
        Some(_) => node_id(cmd, p, "parent")?,
    };
    let index = p.get("index").and_then(Value::as_u64).map(|i| i as usize);
    let id = m.next_node_id();
    for (pt, next) in points.iter_mut().zip(m.next_point_id().0..) {
        pt.id = PointId(next);
    }
    let name = p.get("name").and_then(Value::as_str).map_or_else(|| format!("{default_name}{}", id.0), str::to_string);
    let mut shape = Shape::new(id, &name);
    shape.closed = closed;
    let ids: Vec<u64> = points.iter().map(|pt| pt.id.0).collect();
    shape.points = points;
    m.insert(parent, index, Node::Shape(shape)).map_err(|e| rerr(cmd, e))?;
    Ok(json!({ "id": id.0, "points": ids }))
}

// ---------------------------------------------------------------------------
// Layer-level commands
// ---------------------------------------------------------------------------

fn roto_add(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_id(s, p)?;
    s.edit("Add Roto Mask", |doc, _| {
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if l.locks.all {
            return Err(EngineError::Other(format!("layer \"{}\" is locked", l.name)));
        }
        if l.roto_mask.is_some() {
            return Err(EngineError::Other("layer already has a roto mask".into()));
        }
        l.roto_mask = Some(RotoMask::default());
        Ok(())
    })?;
    roto_info(s, p)
}

fn roto_remove(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_id(s, p)?;
    s.edit("Delete Roto Mask", |doc, _| {
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        l.roto_mask.take().map(|_| ()).ok_or_else(|| EngineError::Other("layer has no roto mask".into()))
    })?;
    Ok(Value::Null)
}

fn roto_info(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_id(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    Ok(match &l.roto_mask {
        Some(m) => info_json(id, m),
        None => json!({ "layer": id.0, "rotoMask": null }),
    })
}

fn instance_set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.instance.set";
    let enabled = opt_bool(CMD, p, "enabled")?;
    let linked = opt_bool(CMD, p, "linked")?;
    let invert = opt_bool(CMD, p, "invert")?;
    let density = unit(CMD, p, "density")?;
    let overlap = match p.get("overlap").and_then(Value::as_str) {
        None => None,
        Some("max") => Some(OverlapMode::Max),
        Some("sum") => Some(OverlapMode::Sum),
        Some("over") => Some(OverlapMode::Over),
        Some(o) => return Err(bad(CMD, format!("unknown overlap mode `{o}` (max, sum, over)"))),
    };
    let backend = match p.get("backend").and_then(Value::as_str) {
        None => None,
        Some("auto") => Some(roto::Backend::Auto),
        Some("cpu") => Some(roto::Backend::Cpu),
        Some("gpu") => Some(roto::Backend::Gpu),
        Some(b) => return Err(bad(CMD, format!("unknown backend `{b}` (auto, cpu, gpu)"))),
    };
    with_roto(s, p, CMD, "Roto Mask Settings", |m| {
        m.enabled = enabled.unwrap_or(m.enabled);
        m.linked = linked.unwrap_or(m.linked);
        m.invert = invert.unwrap_or(m.invert);
        m.density = density.unwrap_or(m.density);
        m.overlap = overlap.unwrap_or(m.overlap);
        m.backend = backend.unwrap_or(m.backend);
        Ok(Value::Null)
    })?;
    roto_info(s, p)
}

/// Layer › Rasterize › Roto Mask: multiplies the roto mask into the pixel mask and removes it.
fn roto_rasterize(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_id(s, p)?;
    s.edit("Rasterize Roto Mask", |doc, _| {
        let area = doc.bounds();
        let depth = doc.depth;
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let rm = l.roto_mask.take().ok_or_else(|| EngineError::Other("layer has no roto mask".into()))?;
        let vals = vector::roto::roto_values(&rm, area);
        let mut mask = l.mask.take().unwrap_or_else(|| {
            let mut m = photocraft_doc::LayerMask::reveal_all();
            m.surface =
                photocraft_raster::Surface::with_default(photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Grayscale, depth, false), &[1.0]);
            m
        });
        let old = mask.surface.read_region(area);
        let merged: Vec<f32> = old.iter().zip(&vals).map(|(a, b)| a * b).collect();
        mask.surface.write_region(area, &merged);
        mask.surface.prune();
        l.mask = Some(mask);
        Ok(())
    })?;
    Ok(json!({ "layer": id.0 }))
}

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

fn add_shape(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.add_shape";
    let arr = p.get("points").and_then(Value::as_array).ok_or_else(|| bad(CMD, "`points` must be an array"))?;
    if arr.len() > roto::MAX_POINTS_PER_SHAPE {
        return Err(bad(CMD, format!("too many points (max {})", roto::MAX_POINTS_PER_SHAPE)));
    }
    let points = arr.iter().map(|v| parse_point(CMD, v, PointId(0))).collect::<Result<Vec<_>>>()?;
    let closed = opt_bool(CMD, p, "closed")?.unwrap_or(true);
    with_roto(s, p, CMD, "Add Roto Shape", |m| add_shape_node(CMD, m, p, "Bezier", closed, points))
}

fn rect_param(cmd: &str, p: &Value) -> Result<[f64; 4]> {
    let a = p.get("rect").and_then(Value::as_array).filter(|a| a.len() == 4).ok_or_else(|| bad(cmd, "`rect` must be [x, y, w, h]"))?;
    let mut r = [0.0; 4];
    for (o, v) in r.iter_mut().zip(a) {
        *o = v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad(cmd, "`rect` must be [x, y, w, h]"))?;
    }
    if r[2] <= 0.0 || r[3] <= 0.0 {
        return Err(bad(cmd, "`rect` needs a positive width and height"));
    }
    Ok(r)
}

fn add_rect(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.add_rect";
    let [x, y, w, h] = rect_param(CMD, p)?;
    let points = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)].iter().map(|&(px, py)| Point::corner(PointId(0), px, py)).collect();
    with_roto(s, p, CMD, "Add Roto Rectangle", |m| add_shape_node(CMD, m, p, "Rectangle", true, points))
}

fn add_ellipse(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.add_ellipse";
    let [x, y, w, h] = rect_param(CMD, p)?;
    let (cx, cy, rx, ry) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
    const KAPPA: f64 = 0.552_284_749_830_793_4;
    let points = [(0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)]
        .iter()
        .map(|&(dx, dy)| {
            let mut pt = Point::corner(PointId(0), cx + dx * rx, cy + dy * ry);
            // Clockwise on screen (y down): the outgoing handle is the quarter turn.
            pt.tangent_out = V2::new(-dy * rx * KAPPA, dx * ry * KAPPA);
            pt.tangent_in = V2::new(dy * rx * KAPPA, -dx * ry * KAPPA);
            pt.smooth = true;
            pt
        })
        .collect();
    with_roto(s, p, CMD, "Add Roto Ellipse", |m| add_shape_node(CMD, m, p, "Ellipse", true, points))
}

/// Largest freehand stroke accepted.
const MAX_FREEHAND_POINTS: usize = 100_000;

/// A Catmull-Rom spline through stroke samples (open, or periodic when closed), parameterized
/// over `0..1`, as input to kurbo's curve fitter.
struct Stroke {
    pts: Vec<kurbo::Point>,
    closed: bool,
}

impl Stroke {
    fn segments(&self) -> usize {
        if self.closed { self.pts.len() } else { self.pts.len().saturating_sub(1) }
    }

    fn at(&self, i: isize) -> kurbo::Point {
        let n = self.pts.len() as isize;
        let k = if self.closed { i.rem_euclid(n) } else { i.clamp(0, n - 1) };
        self.pts.get(k as usize).copied().unwrap_or(kurbo::Point::ZERO)
    }

    /// Point and derivative with respect to `t` in `0..1`.
    fn eval(&self, t: f64) -> (kurbo::Point, kurbo::Vec2) {
        let segs = self.segments().max(1) as f64;
        let u = (t.clamp(0.0, 1.0) * segs).min(segs - f64::EPSILON * segs);
        let i = u.floor();
        let s = u - i;
        let i = i as isize;
        let (p0, p1, p2, p3) = (self.at(i - 1).to_vec2(), self.at(i).to_vec2(), self.at(i + 1).to_vec2(), self.at(i + 2).to_vec2());
        let a = p1 * 2.0;
        let b = p2 - p0;
        let c = p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3;
        let d = -p0 + p1 * 3.0 - p2 * 3.0 + p3;
        let pos = (a + b * s + c * (s * s) + d * (s * s * s)) * 0.5;
        let der = (b + c * (2.0 * s) + d * (3.0 * s * s)) * 0.5 * segs;
        (pos.to_point(), der)
    }
}

impl kurbo::ParamCurveFit for Stroke {
    fn sample_pt_tangent(&self, t: f64, _sign: f64) -> kurbo::CurveFitSample {
        let (p, tangent) = self.eval(t);
        kurbo::CurveFitSample { p, tangent }
    }

    fn sample_pt_deriv(&self, t: f64) -> (kurbo::Point, kurbo::Vec2) {
        self.eval(t)
    }

    // Catmull-Rom is C1 and consecutive duplicates are removed beforehand: no cusps.
    fn break_cusp(&self, _range: std::ops::Range<f64>) -> Option<f64> {
        None
    }
}

/// Fits a freehand stroke to bezier points: a Catmull-Rom spline through the samples, reduced by
/// kurbo's curve fitter to within `tolerance` px. Returns `None` when the stroke has no extent.
fn fit_stroke(samples: &[V2], closed: bool, tolerance: f64) -> Option<Vec<Point>> {
    let mut pts: Vec<kurbo::Point> = Vec::with_capacity(samples.len());
    for q in samples {
        let k = kurbo::Point::new(q.x, q.y);
        if pts.last().is_none_or(|l| l.distance(k) > 1e-9) {
            pts.push(k);
        }
    }
    if closed && pts.len() > 1 && pts.first().zip(pts.last()).is_some_and(|(a, b)| a.distance(*b) <= 1e-9) {
        pts.pop();
    }
    if pts.len() < 2 {
        return None;
    }
    let stroke = Stroke { pts, closed };
    let fitted = kurbo::fit_to_bezpath(&stroke, tolerance);
    let mut points: Vec<Point> = Vec::new();
    for el in fitted.elements() {
        match *el {
            kurbo::PathEl::MoveTo(a) | kurbo::PathEl::LineTo(a) => points.push(Point::corner(PointId(0), a.x, a.y)),
            kurbo::PathEl::CurveTo(c1, c2, a) => {
                if let Some(prev) = points.last_mut() {
                    prev.tangent_out = V2::new(c1.x - prev.pos.x, c1.y - prev.pos.y);
                }
                let mut pt = Point::corner(PointId(0), a.x, a.y);
                pt.tangent_in = V2::new(c2.x - a.x, c2.y - a.y);
                points.push(pt);
            }
            kurbo::PathEl::QuadTo(c, a) => {
                // Elevate to a cubic: control points two thirds of the way from each end to `c`.
                if let Some(prev) = points.last_mut() {
                    prev.tangent_out = V2::new((c.x - prev.pos.x) * 2.0 / 3.0, (c.y - prev.pos.y) * 2.0 / 3.0);
                }
                let mut pt = Point::corner(PointId(0), a.x, a.y);
                pt.tangent_in = V2::new((c.x - a.x) * 2.0 / 3.0, (c.y - a.y) * 2.0 / 3.0);
                points.push(pt);
            }
            kurbo::PathEl::ClosePath => {}
        }
    }
    if closed && points.len() > 2 {
        // The fit ends where it began: merge the duplicate end point into the first.
        let near = points.first().zip(points.last()).is_some_and(|(a, b)| (a.pos.x - b.pos.x).hypot(a.pos.y - b.pos.y) < 1e-6);
        if near
            && let Some(last) = points.pop()
            && let Some(first) = points.first_mut()
        {
            first.tangent_in = last.tangent_in;
            first.smooth = true;
        }
    }
    (points.len() >= 2).then_some(points)
}

fn add_freehand(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.add_freehand";
    let arr = p.get("points").and_then(Value::as_array).ok_or_else(|| bad(CMD, "`points` must be an array of [x, y]"))?;
    if arr.len() < 2 || arr.len() > MAX_FREEHAND_POINTS {
        return Err(bad(CMD, format!("`points` needs 2 to {MAX_FREEHAND_POINTS} entries")));
    }
    let pts = arr.iter().map(|v| v2(v).ok_or_else(|| bad(CMD, "`points` must be an array of [x, y]"))).collect::<Result<Vec<_>>>()?;
    let tolerance = opt_f64(CMD, p, "tolerance")?.unwrap_or(2.0).clamp(0.01, 1000.0);
    let closed = opt_bool(CMD, p, "closed")?.unwrap_or(true);
    let points = fit_stroke(&pts, closed, tolerance).ok_or_else(|| bad(CMD, "the stroke has no extent"))?;
    with_roto(s, p, CMD, "Add Roto Freehand", |m| add_shape_node(CMD, m, p, "Freehand", closed, points))
}

fn node_delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.delete";
    let ids = node_ids(CMD, p, "ids")?;
    with_roto(s, p, CMD, "Delete Roto Nodes", |m| {
        for id in &ids {
            if m.find(*id).is_none() {
                return Err(bad(CMD, format!("no node with id {}", id.0)));
            }
        }
        // A parent removed earlier in the list takes its children with it; that is not an error.
        for id in &ids {
            m.remove(*id);
        }
        Ok(json!({ "removed": ids.len() }))
    })
}

fn node_rename(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.rename";
    let id = node_id(CMD, p, "id")?;
    let name = p
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty() && n.len() <= 256)
        .ok_or_else(|| bad(CMD, "`name` must be 1 to 256 characters"))?
        .to_string();
    with_roto(s, p, CMD, "Rename Roto Node", |m| match m.find_mut(id) {
        Some(Node::Shape(sh)) => {
            sh.name = name;
            Ok(Value::Null)
        }
        Some(Node::Group(g)) => {
            g.name = name;
            Ok(Value::Null)
        }
        None => Err(bad(CMD, format!("no node with id {}", id.0))),
    })
}

fn node_reorder(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.reorder";
    let id = node_id(CMD, p, "id")?;
    let index = p.get("index").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "`index` must be a non-negative integer"))? as usize;
    with_roto(s, p, CMD, "Reorder Roto Node", |m| {
        m.reorder(id, index).map_err(|e| rerr(CMD, e))?;
        Ok(Value::Null)
    })
}

fn node_group(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.group";
    let ids = node_ids(CMD, p, "ids")?;
    let name = p.get("name").and_then(Value::as_str).unwrap_or("Group").to_string();
    with_roto(s, p, CMD, "Group Roto Nodes", |m| {
        let id = m.group(&ids, &name).map_err(|e| rerr(CMD, e))?;
        Ok(json!({ "id": id.0 }))
    })
}

fn node_ungroup(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.ungroup";
    let id = node_id(CMD, p, "id")?;
    with_roto(s, p, CMD, "Ungroup Roto Nodes", |m| {
        let ids = m.ungroup(id).map_err(|e| rerr(CMD, e))?;
        Ok(json!({ "ids": ids.iter().map(|i| i.0).collect::<Vec<_>>() }))
    })
}

fn node_duplicate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.duplicate";
    let ids = node_ids(CMD, p, "ids")?;
    with_roto(s, p, CMD, "Duplicate Roto Nodes", |m| {
        let copies = m.duplicate(&ids).map_err(|e| rerr(CMD, e))?;
        Ok(json!({ "ids": copies.iter().map(|i| i.0).collect::<Vec<_>>() }))
    })
}

/// An optional colour param: absent leaves it alone, `null` resets it, `[r, g, b]` (0-255) sets it.
fn opt_color(cmd: &str, p: &Value, key: &str) -> Result<Option<Option<[u8; 3]>>> {
    match p.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::Array(a)) if a.len() == 3 => {
            let mut c = [0u8; 3];
            for (o, v) in c.iter_mut().zip(a) {
                *o = v.as_u64().filter(|n| *n <= 255).ok_or_else(|| bad(cmd, format!("`{key}` must be [r, g, b] with 0-255 values, or null")))? as u8;
            }
            Ok(Some(Some(c)))
        }
        Some(_) => Err(bad(cmd, format!("`{key}` must be [r, g, b] with 0-255 values, or null"))),
    }
}

fn node_set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.set";
    let id = node_id(CMD, p, "id")?;
    let visible = opt_bool(CMD, p, "visible")?;
    let locked = opt_bool(CMD, p, "locked")?;
    let opacity = unit(CMD, p, "opacity")?;
    let blend = match p.get("blendOp").and_then(Value::as_str) {
        None => None,
        Some(b) => Some(blend_op(b).ok_or_else(|| bad(CMD, format!("unknown blendOp `{b}`")))?),
    };
    let invert = opt_bool(CMD, p, "invert")?;
    let closed = opt_bool(CMD, p, "closed")?;
    let color = opt_color(CMD, p, "color")?;
    let blur = match opt_f64(CMD, p, "blur")? {
        None => None,
        Some(b) if (0.0..=f64::from(MAX_BLUR)).contains(&b) => Some(b as f32),
        Some(_) => return Err(bad(CMD, format!("`blur` must be between 0 and {MAX_BLUR}"))),
    };
    let fall = match p.get("falloff").and_then(Value::as_str) {
        None => None,
        Some(f) => Some(falloff(f).ok_or_else(|| bad(CMD, format!("unknown falloff `{f}` (linear, smooth, easeIn, easeOut)")))?),
    };
    with_roto(s, p, CMD, "Roto Node Settings", |m| {
        match m.find_mut(id) {
            Some(Node::Shape(sh)) => {
                sh.visible = visible.unwrap_or(sh.visible);
                sh.locked = locked.unwrap_or(sh.locked);
                sh.opacity = opacity.unwrap_or(sh.opacity);
                sh.blend_op = blend.unwrap_or(sh.blend_op);
                sh.invert = invert.unwrap_or(sh.invert);
                sh.closed = closed.unwrap_or(sh.closed);
                sh.color = color.unwrap_or(sh.color);
                sh.blur = blur.unwrap_or(sh.blur);
                sh.falloff = fall.unwrap_or(sh.falloff);
            }
            Some(Node::Group(g)) => {
                if invert.is_some() || blur.is_some() || fall.is_some() {
                    return Err(bad(CMD, "`invert`, `blur` and `falloff` apply to shapes, not groups"));
                }
                g.visible = visible.unwrap_or(g.visible);
                g.locked = locked.unwrap_or(g.locked);
                g.opacity = opacity.unwrap_or(g.opacity);
                g.blend_op = blend.unwrap_or(g.blend_op);
            }
            None => return Err(bad(CMD, format!("no node with id {}", id.0))),
        }
        Ok(Value::Null)
    })
}

fn node_transform(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.node.transform";
    let id = node_id(CMD, p, "id")?;
    let translate = opt_v2(CMD, p, "translate")?;
    let scale = opt_v2(CMD, p, "scale")?;
    let pivot = opt_v2(CMD, p, "pivot")?;
    let rotate = opt_f64(CMD, p, "rotate")?;
    let skew = opt_f64(CMD, p, "skew")?;
    if skew.is_some_and(|k| k.abs() >= 89.0) {
        return Err(bad(CMD, "`skew` must be between -89 and 89 degrees"));
    }
    let apply = move |t: &mut roto::Transform2D| {
        t.translate = translate.unwrap_or(t.translate);
        t.scale = scale.unwrap_or(t.scale);
        t.pivot = pivot.unwrap_or(t.pivot);
        t.rotate = rotate.unwrap_or(t.rotate);
        t.skew = skew.unwrap_or(t.skew);
    };
    with_roto(s, p, CMD, "Transform Roto Node", |m| {
        if id == m.root.id {
            apply(&mut m.root.transform);
            return Ok(Value::Null);
        }
        match m.find_mut(id) {
            Some(Node::Shape(sh)) => apply(&mut sh.transform),
            Some(Node::Group(g)) => apply(&mut g.transform),
            None => return Err(bad(CMD, format!("no node with id {}", id.0))),
        }
        Ok(Value::Null)
    })
}

// ---------------------------------------------------------------------------
// Points
// ---------------------------------------------------------------------------

fn point_add(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.point.add";
    let shape = node_id(CMD, p, "shape")?;
    let pos = req_v2(CMD, p, "pos")?;
    let index = p.get("index").and_then(Value::as_u64).map(|i| i as usize);
    let smooth = opt_bool(CMD, p, "smooth")?.unwrap_or(false);
    with_roto(s, p, CMD, "Add Roto Point", |m| {
        let id = m.next_point_id();
        let sh = shape_of(CMD, m, shape)?;
        if sh.points.len() >= roto::MAX_POINTS_PER_SHAPE {
            return Err(bad(CMD, "the shape has the maximum number of points"));
        }
        let mut pt = Point::corner(id, pos.x, pos.y);
        pt.smooth = smooth;
        let at = index.map_or(sh.points.len(), |i| i.min(sh.points.len()));
        sh.points.insert(at, pt);
        Ok(json!({ "id": id.0, "index": at }))
    })
}

fn point_delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.point.delete";
    let shape = node_id(CMD, p, "shape")?;
    let ids = point_ids(CMD, p, "ids")?.ok_or_else(|| bad(CMD, "`ids` must be an array of point ids"))?;
    with_roto(s, p, CMD, "Delete Roto Points", |m| {
        let sh = shape_of(CMD, m, shape)?;
        for id in &ids {
            if !sh.points.iter().any(|q| q.id == *id) {
                return Err(bad(CMD, format!("no point with id {}", id.0)));
            }
        }
        sh.points.retain(|q| !ids.contains(&q.id));
        Ok(json!({ "points": sh.points.len() }))
    })
}

fn point_move(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.point.move";
    let shape = node_id(CMD, p, "shape")?;
    let moves = p.get("moves").and_then(Value::as_array).ok_or_else(|| bad(CMD, "`moves` must be an array of {id, pos}"))?;
    let moves = moves
        .iter()
        .map(|mv| {
            let id = mv.get("id").and_then(Value::as_u64).map(PointId).ok_or_else(|| bad(CMD, "each move needs a point `id`"))?;
            Ok((id, req_v2(CMD, mv, "pos")?))
        })
        .collect::<Result<Vec<_>>>()?;
    with_roto(s, p, CMD, "Move Roto Points", |m| {
        let sh = shape_of(CMD, m, shape)?;
        for (id, pos) in &moves {
            let q = sh.points.iter_mut().find(|q| q.id == *id).ok_or_else(|| bad(CMD, format!("no point with id {}", id.0)))?;
            q.pos = *pos;
        }
        Ok(Value::Null)
    })
}

fn point_set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.point.set";
    let shape = node_id(CMD, p, "shape")?;
    let id = p.get("id").and_then(Value::as_u64).map(PointId).ok_or_else(|| bad(CMD, "`id` must be a point id"))?;
    let smooth = opt_bool(CMD, p, "smooth")?;
    let tin = opt_v2(CMD, p, "in")?;
    let tout = opt_v2(CMD, p, "out")?;
    let feather = opt_v2(CMD, p, "feather")?;
    let fin = opt_v2(CMD, p, "featherIn")?;
    let fout = opt_v2(CMD, p, "featherOut")?;
    with_roto(s, p, CMD, "Edit Roto Point", |m| {
        let sh = shape_of(CMD, m, shape)?;
        let q = sh.points.iter_mut().find(|q| q.id == id).ok_or_else(|| bad(CMD, format!("no point with id {}", id.0)))?;
        q.smooth = smooth.unwrap_or(q.smooth);
        if let Some(t) = tin {
            q.tangent_in = t;
        }
        if let Some(t) = tout {
            q.tangent_out = t;
        }
        // A smooth point keeps its handles collinear: moving one mirrors the other unless both are given.
        if q.smooth {
            match (tin, tout) {
                (Some(t), None) => q.tangent_out = V2::new(-t.x, -t.y),
                (None, Some(t)) => q.tangent_in = V2::new(-t.x, -t.y),
                _ => {}
            }
        }
        q.feather_pos = feather.unwrap_or(q.feather_pos);
        q.feather_in = fin.unwrap_or(q.feather_in);
        q.feather_out = fout.unwrap_or(q.feather_out);
        // The feather point's handles stay collinear on a smooth point too: moving one swings the
        // other to the opposite side (relative to the point's own handle, scaled to its length).
        if q.smooth {
            let (len, mirror) = (|v: V2| v.x.hypot(v.y), |v: V2, k: f64| V2::new(-v.x * k, -v.y * k));
            let eff = |t: V2, f: V2| V2::new(t.x + f.x, t.y + f.y);
            let ratio = |own: V2, other: V2| if len(own) > 1e-12 { len(other) / len(own) } else { 1.0 };
            match (fin, fout) {
                (Some(f), None) => {
                    let e = mirror(eff(q.tangent_in, f), ratio(q.tangent_in, q.tangent_out));
                    q.feather_out = V2::new(e.x - q.tangent_out.x, e.y - q.tangent_out.y);
                }
                (None, Some(f)) => {
                    let e = mirror(eff(q.tangent_out, f), ratio(q.tangent_out, q.tangent_in));
                    q.feather_in = V2::new(e.x - q.tangent_in.x, e.y - q.tangent_in.y);
                }
                _ => {}
            }
        }
        Ok(Value::Null)
    })
}

/// Bakes a relative transform into points: positions map through it, handles and feather offsets
/// through its linear part.
fn point_transform(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.point.transform";
    let shape = node_id(CMD, p, "shape")?;
    let ids = point_ids(CMD, p, "ids")?;
    let t = roto::Transform2D {
        translate: opt_v2(CMD, p, "translate")?.unwrap_or(V2::ZERO),
        rotate: opt_f64(CMD, p, "rotate")?.unwrap_or(0.0),
        scale: opt_v2(CMD, p, "scale")?.unwrap_or(V2::new(1.0, 1.0)),
        skew: opt_f64(CMD, p, "skew")?.unwrap_or(0.0),
        pivot: opt_v2(CMD, p, "pivot")?.unwrap_or(V2::ZERO),
    };
    if t.skew.abs() >= 89.0 {
        return Err(bad(CMD, "`skew` must be between -89 and 89 degrees"));
    }
    with_roto(s, p, CMD, "Transform Roto Points", |m| {
        let sh = shape_of(CMD, m, shape)?;
        if let Some(ids) = &ids {
            for id in ids {
                if !sh.points.iter().any(|q| q.id == *id) {
                    return Err(bad(CMD, format!("no point with id {}", id.0)));
                }
            }
        }
        let mut moved = 0usize;
        for q in &mut sh.points {
            if ids.as_ref().is_some_and(|ids| !ids.contains(&q.id)) {
                continue;
            }
            q.pos = t.apply(q.pos);
            q.tangent_in = t.apply_vec(q.tangent_in);
            q.tangent_out = t.apply_vec(q.tangent_out);
            q.feather_pos = t.apply_vec(q.feather_pos);
            q.feather_in = t.apply_vec(q.feather_in);
            q.feather_out = t.apply_vec(q.feather_out);
            moved += 1;
        }
        Ok(json!({ "moved": moved }))
    })
}

/// Cusp or smooth the listed points (default: all) of a shape. See `Shape::cusp_points` and
/// `Shape::smooth_points` for exactly what each does.
fn point_shape_op(s: &mut Session, p: &Value, cmd: &'static str, label: &str, op: fn(&mut Shape, Option<&[PointId]>) -> usize) -> Result<Value> {
    let shape = node_id(cmd, p, "shape")?;
    let ids = point_ids(cmd, p, "ids")?;
    with_roto(s, p, cmd, label, |m| {
        let sh = shape_of(cmd, m, shape)?;
        if let Some(ids) = &ids {
            for id in ids {
                if !sh.points.iter().any(|q| q.id == *id) {
                    return Err(bad(cmd, format!("no point with id {}", id.0)));
                }
            }
        }
        Ok(json!({ "changed": op(sh, ids.as_deref()) }))
    })
}

fn point_cusp(s: &mut Session, p: &Value) -> Result<Value> {
    point_shape_op(s, p, "roto.point.cusp", "Cusp Roto Points", Shape::cusp_points)
}

fn point_smooth(s: &mut Session, p: &Value) -> Result<Value> {
    point_shape_op(s, p, "roto.point.smooth", "Smooth Roto Points", Shape::smooth_points)
}

// ---------------------------------------------------------------------------
// Selection from the mask

/// Ids of every shape at or under `n` (depth-capped: a valid mask is shallow anyway).
fn shape_ids_under(n: &Node, depth: usize, out: &mut Vec<NodeId>) {
    match n {
        Node::Shape(sh) => out.push(sh.id),
        Node::Group(g) if depth < 64 => g.children.iter().for_each(|c| shape_ids_under(c, depth + 1, out)),
        Node::Group(_) => {}
    }
}

/// `m` with only the shapes in `keep` visible (and those forced visible; enclosing groups keep
/// their own visibility and transforms).
fn only_shapes(m: &RotoMask, keep: &[NodeId]) -> RotoMask {
    fn walk(n: &mut Node, keep: &[NodeId], depth: usize) {
        match n {
            Node::Shape(sh) => sh.visible = keep.contains(&sh.id),
            Node::Group(g) if depth < 64 => g.children.iter_mut().for_each(|c| walk(c, keep, depth + 1)),
            Node::Group(_) => {}
        }
    }
    let mut out = m.clone();
    out.enabled = true;
    out.root.children.iter_mut().for_each(|c| walk(c, keep, 0));
    out
}

/// Makes the document selection from the mask: the whole mask as drawn (`node` 0, the default),
/// or just one group or shape. With `"each": true` every shape under the node is evaluated on its
/// own (ignoring the blend ops between them) and the results are unioned, so a Subtract shape does
/// not cut its neighbours but adds its own area.
fn make_selection(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.selection.make";
    let id = layer_id(s, p)?;
    let node = match p.get("node") {
        None | Some(Value::Null) => NodeId(0),
        Some(_) => node_id(CMD, p, "node")?,
    };
    let each = opt_bool(CMD, p, "each")?.unwrap_or(false);
    let mode = match p.get("mode").and_then(Value::as_str).unwrap_or("replace") {
        m @ ("replace" | "add" | "subtract" | "intersect") => m,
        m => return Err(bad(CMD, format!("unknown mode `{m}` (replace, add, subtract, intersect)"))),
    };
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let area = d.doc.bounds();
    let m = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?.roto_mask.as_ref().ok_or_else(|| EngineError::Other("layer has no roto mask".into()))?;
    let mut ids = Vec::new();
    if node == NodeId(0) || node == m.root.id {
        m.root.children.iter().for_each(|c| shape_ids_under(c, 0, &mut ids));
    } else {
        shape_ids_under(m.find(node).ok_or_else(|| bad(CMD, format!("no node with id {}", node.0)))?, 0, &mut ids);
    }
    if ids.is_empty() {
        return Err(bad(CMD, "there are no shapes to make a selection from"));
    }
    let n = area.width() as usize * area.height() as usize;
    let whole = node == NodeId(0) || node == m.root.id;
    let values = if each {
        let mut acc = vec![0.0f32; n];
        for sid in &ids {
            let v = vector::roto::roto_values_auto(&only_shapes(m, &[*sid]), area);
            if v.len() != n {
                return Err(bad(CMD, "the document is too large to evaluate the mask"));
            }
            acc.iter_mut().zip(v).for_each(|(a, b)| *a = a.max(b));
        }
        acc
    } else if whole {
        vector::roto::roto_values_auto(m, area)
    } else {
        vector::roto::roto_values_auto(&only_shapes(m, &ids), area)
    };
    if values.len() != n {
        return Err(bad(CMD, "the document is too large to evaluate the mask"));
    }
    let mode = photocraft_algo::selection::SelectionMode::parse(mode);
    let selected = s.edit("Selection from Roto Mask", |doc, _| {
        doc.selection = photocraft_algo::selection::combine(doc.selection.as_ref(), &values, area, mode);
        Ok(doc.selection.is_some())
    })?;
    Ok(json!({ "selected": selected, "shapes": ids.len() }))
}

/// Most shapes one selection may become: a speckled selection would otherwise bury the mask.
const MAX_SELECTION_SHAPES: usize = 500;

/// Even-odd point in polygon.
fn inside_polygon(poly: &[(f64, f64)], (px, py): (f64, f64)) -> bool {
    let mut inside = false;
    for (i, &(x1, y1)) in poly.iter().enumerate() {
        let (x2, y2) = poly.get((i + 1) % poly.len()).copied().unwrap_or((x1, y1));
        if (y1 > py) != (y2 > py) && px < (x2 - x1) * (py - y1) / (y2 - y1) + x1 {
            inside = !inside;
        }
    }
    inside
}

/// Adds bezier shapes that follow the document selection's outline to the layer's roto mask
/// (creating the mask when the layer has none). Each region becomes a shape; holes (and islands
/// inside holes, and so on) alternate Subtract and Union so the mask has the selection's shape.
/// Several shapes are put in one group.
fn selection_to_shapes(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.selection.to_shapes";
    let id = layer_id(s, p)?;
    let tolerance = opt_f64(CMD, p, "tolerance")?.unwrap_or(2.0).clamp(0.5, 10.0);
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let area = d.doc.bounds();
    if d.doc.selection.is_none() {
        return Err(bad(CMD, "there is no selection"));
    }
    let values = photocraft_algo::selection::mask_from_surface(d.doc.selection.as_ref(), area);
    let path = vector::trace::trace_mask(&values, area, 0.0, tolerance);
    if path.subpaths.is_empty() {
        return Err(bad(CMD, "the selection has no outline"));
    }
    if path.subpaths.len() > MAX_SELECTION_SHAPES {
        return Err(bad(
            CMD,
            format!("the selection has {} separate outlines (at most {MAX_SELECTION_SHAPES}); smooth it or raise `tolerance`", path.subpaths.len()),
        ));
    }
    let polys: Vec<Vec<(f64, f64)>> = path.subpaths.iter().map(|sp| sp.knots.iter().map(|k| (k.anchor.x, k.anchor.y)).collect()).collect();
    let mut order: Vec<(usize, usize)> = (0..polys.len())
        .map(|i| {
            let probe = polys[i].first().copied().unwrap_or((0.0, 0.0));
            (polys.iter().enumerate().filter(|(j, poly)| *j != i && poly.len() >= 3 && inside_polygon(poly, probe)).count(), i)
        })
        .collect();
    order.sort_unstable();
    let prefix = p.get("name").and_then(Value::as_str).unwrap_or("Selection").to_string();
    let created = s.edit("Roto Shapes from Selection", |doc, _| {
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if l.locks.all {
            return Err(EngineError::Other(format!("layer \"{}\" is locked", l.name)));
        }
        let m = l.roto_mask.get_or_insert_with(RotoMask::default);
        let mut ids = Vec::new();
        for (n, (depth, i)) in order.iter().enumerate() {
            let Some(sp) = path.subpaths.get(*i).filter(|sp| sp.knots.len() >= 3) else { continue };
            let nid = m.next_node_id();
            let first_point = m.next_point_id().0;
            let mut shape = Shape::new(nid, &format!("{prefix} {}", n + 1));
            shape.closed = true;
            if depth % 2 == 1 {
                shape.blend_op = BlendOp::Subtract;
            }
            for (next, k) in (first_point..).zip(sp.knots.iter()) {
                let mut pt = Point::corner(PointId(next), k.anchor.x, k.anchor.y);
                pt.tangent_in = V2::new(k.in_ctrl.x - k.anchor.x, k.in_ctrl.y - k.anchor.y);
                pt.tangent_out = V2::new(k.out_ctrl.x - k.anchor.x, k.out_ctrl.y - k.anchor.y);
                pt.smooth = k.smooth;
                shape.points.push(pt);
            }
            let root = m.root.id;
            m.insert(root, None, Node::Shape(shape)).map_err(|e| rerr(CMD, e))?;
            ids.push(nid);
        }
        if ids.is_empty() {
            return Err(bad(CMD, "the selection has no usable outline"));
        }
        let group = if ids.len() > 1 { Some(m.group(&ids, &prefix).map_err(|e| rerr(CMD, e))?.0) } else { None };
        m.validate().map_err(|e| rerr(CMD, e))?;
        Ok((ids.iter().map(|i| i.0).collect::<Vec<_>>(), group))
    })?;
    Ok(json!({ "shapes": created.0, "group": created.1 }))
}

/// Exports the mask (or one node of it) as an ordinary path: the work path, or a saved path in the
/// Paths panel when `name` is given (replacing a saved path of that name).
fn export_path_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.export_path";
    let id = layer_id(s, p)?;
    let node = match p.get("node") {
        None | Some(Value::Null) => None,
        Some(_) => Some(node_id(CMD, p, "node")?),
    };
    let name = p.get("name").and_then(Value::as_str).map(str::to_string);
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let m = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?.roto_mask.as_ref().ok_or_else(|| EngineError::Other("layer has no roto mask".into()))?;
    let path = vector::roto::export_path(m, node).ok_or_else(|| bad(CMD, "no such node"))?;
    if path.subpaths.is_empty() {
        return Err(bad(CMD, "there are no visible shapes to export"));
    }
    let subpaths = path.subpaths.len();
    s.edit("Roto to Path", |doc, _| {
        match &name {
            None => doc.work_path = Some(path),
            Some(n) => match doc.paths.iter_mut().find(|q| q.name == *n) {
                Some(q) => {
                    q.path = path;
                    q.psd_raw = None;
                }
                None => doc.paths.push(photocraft_doc::NamedPath { name: n.clone(), path, psd_raw: None }),
            },
        }
        Ok(())
    })?;
    Ok(json!({ "subpaths": subpaths, "name": name }))
}

// ---------------------------------------------------------------------------
// Feather
// ---------------------------------------------------------------------------

fn feather_set_point(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.feather.set_point";
    let shape = node_id(CMD, p, "shape")?;
    let id = p.get("id").and_then(Value::as_u64).map(PointId).ok_or_else(|| bad(CMD, "`id` must be a point id"))?;
    let offset = req_v2(CMD, p, "offset")?;
    with_roto(s, p, CMD, "Feather Roto Point", |m| {
        let sh = shape_of(CMD, m, shape)?;
        let q = sh.points.iter_mut().find(|q| q.id == id).ok_or_else(|| bad(CMD, format!("no point with id {}", id.0)))?;
        q.feather_pos = offset;
        Ok(Value::Null)
    })
}

/// Pulls the feather of the listed points (default: all) out by `factor`: the offset and the
/// relative feather tangents scale together, so an even feather stays even.
fn feather_scale_selected(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.feather.scale_selected";
    let shape = node_id(CMD, p, "shape")?;
    let ids = point_ids(CMD, p, "ids")?;
    let factor = opt_f64(CMD, p, "factor")?.ok_or_else(|| bad(CMD, "`factor` is required"))?;
    if !(-1000.0..=1000.0).contains(&factor) {
        return Err(bad(CMD, "`factor` must be between -1000 and 1000"));
    }
    with_roto(s, p, CMD, "Scale Roto Feather", |m| {
        let sh = shape_of(CMD, m, shape)?;
        if let Some(ids) = &ids {
            for id in ids {
                if !sh.points.iter().any(|q| q.id == *id) {
                    return Err(bad(CMD, format!("no point with id {}", id.0)));
                }
            }
        }
        let mut changed = 0usize;
        for q in &mut sh.points {
            if ids.as_ref().is_some_and(|ids| !ids.contains(&q.id)) {
                continue;
            }
            let sc = |v: V2| V2::new(v.x * factor, v.y * factor);
            q.feather_pos = sc(q.feather_pos);
            q.feather_in = sc(q.feather_in);
            q.feather_out = sc(q.feather_out);
            changed += 1;
        }
        Ok(json!({ "changed": changed }))
    })
}

/// Unit outward normals per point (away from the centroid), from the neighbouring chord.
fn outward_normals(points: &[Point], closed: bool) -> Vec<V2> {
    let n = points.len();
    if n < 2 {
        return vec![V2::ZERO; n];
    }
    let (cx, cy) = (points.iter().map(|q| q.pos.x).sum::<f64>() / n as f64, points.iter().map(|q| q.pos.y).sum::<f64>() / n as f64);
    (0..n)
        .map(|k| {
            let prev = if closed { (k + n - 1) % n } else { k.saturating_sub(1) };
            let next = if closed { (k + 1) % n } else { (k + 1).min(n - 1) };
            let (Some(a), Some(b), Some(c)) = (points.get(prev), points.get(next), points.get(k)) else { return V2::ZERO };
            let chord = (b.pos.x - a.pos.x, b.pos.y - a.pos.y);
            let len = chord.0.hypot(chord.1);
            if len < 1e-12 {
                return V2::ZERO;
            }
            let mut nrm = (chord.1 / len, -chord.0 / len);
            if nrm.0 * (c.pos.x - cx) + nrm.1 * (c.pos.y - cy) < 0.0 {
                nrm = (-nrm.0, -nrm.1);
            }
            V2::new(nrm.0, nrm.1)
        })
        .collect()
}

/// Sets every point of a shape to feather by `distance` px along its outward normal (negative =
/// inward; 0 clears), scaling the feather tangents so the feather outline is an even offset.
fn set_even_feather(sh: &mut Shape, distance: f64) {
    let normals = outward_normals(&sh.points, sh.closed);
    for (q, n) in sh.points.iter_mut().zip(&normals) {
        q.feather_pos = V2::new(n.x * distance, n.y * distance);
        q.feather_in = V2::ZERO;
        q.feather_out = V2::ZERO;
    }
    if distance == 0.0 {
        return;
    }
    let count = sh.points.len();
    let ratios: Vec<f64> = (0..count)
        .map(|k| {
            let prev = if sh.closed { (k + count - 1) % count } else { k.saturating_sub(1) };
            let next = if sh.closed { (k + 1) % count } else { (k + 1).min(count - 1) };
            let (Some(a), Some(b)) = (sh.points.get(prev), sh.points.get(next)) else { return 1.0 };
            let shape_chord = (b.pos.x - a.pos.x).hypot(b.pos.y - a.pos.y);
            let fa = V2::new(a.pos.x + a.feather_pos.x, a.pos.y + a.feather_pos.y);
            let fb = V2::new(b.pos.x + b.feather_pos.x, b.pos.y + b.feather_pos.y);
            let feather_chord = (fb.x - fa.x).hypot(fb.y - fa.y);
            if shape_chord < 1e-9 { 1.0 } else { feather_chord / shape_chord }
        })
        .collect();
    for (q, r) in sh.points.iter_mut().zip(ratios) {
        let k = r - 1.0;
        q.feather_in = V2::new(q.tangent_in.x * k, q.tangent_in.y * k);
        q.feather_out = V2::new(q.tangent_out.x * k, q.tangent_out.y * k);
    }
}

fn collect_shapes(g: &mut Group, out: &mut Vec<NodeId>) {
    for n in &mut g.children {
        match n {
            Node::Shape(s) => out.push(s.id),
            Node::Group(c) => collect_shapes(c, out),
        }
    }
}

fn shapes_under(m: &mut RotoMask, id: NodeId, out: &mut Vec<NodeId>) -> bool {
    if id == m.root.id {
        collect_shapes(&mut m.root, out);
        return true;
    }
    match m.find_mut(id) {
        Some(Node::Shape(s)) => {
            out.push(s.id);
            true
        }
        Some(Node::Group(g)) => {
            collect_shapes(g, out);
            true
        }
        None => false,
    }
}

/// Feather every point of the listed shapes or groups (default: the whole mask) at once.
fn feather_set_all(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.feather.set_all";
    let distance = opt_f64(CMD, p, "distance")?.ok_or_else(|| bad(CMD, "`distance` is required"))?;
    if distance.abs() > roto::MAX_FEATHER {
        return Err(bad(CMD, format!("`distance` must be within ±{}", roto::MAX_FEATHER)));
    }
    let targets = match p.get("ids") {
        None | Some(Value::Null) => None,
        Some(_) => Some(node_ids(CMD, p, "ids")?),
    };
    with_roto(s, p, CMD, "Feather All Roto Points", |m| {
        let mut shapes = Vec::new();
        match &targets {
            None => {
                let root = m.root.id;
                shapes_under(m, root, &mut shapes);
            }
            Some(ids) => {
                for id in ids {
                    if !shapes_under(m, *id, &mut shapes) {
                        return Err(bad(CMD, format!("no node with id {}", id.0)));
                    }
                }
            }
        }
        let mut done = 0usize;
        for id in shapes {
            if let Some(sh) = m.shape_mut(id) {
                set_even_feather(sh, distance);
                done += 1;
            }
        }
        Ok(json!({ "shapes": done }))
    })
}

// ---------------------------------------------------------------------------
// Editing view

/// `view.rotoEdit`: while a layer's roto mask is being edited it is not applied to the layer's
/// pixels (the editor shows the whole image and draws the mask as an overlay, so the user can see
/// what they are tracing). View state like the Channels panel's eyes: no history step, a clean
/// document stays clean, nothing is saved.
fn view_edit(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "view.rotoEdit";
    let layer = layer_id(s, p)?;
    let want = match p.get("on") {
        None | Some(Value::Null) => !vector::roto::is_editing(layer),
        Some(v) => v.as_bool().ok_or_else(|| bad(CMD, "`on` must be true or false"))?,
    };
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let l = st.doc.layer(layer).ok_or(EngineError::NoLayer(layer))?;
    if want && l.roto_mask.is_none() {
        return Err(bad(CMD, format!("layer \"{}\" has no roto mask", l.name)));
    }
    let editing = vector::roto::is_editing(layer);
    // Turning off for a layer that is not the one being edited leaves the other edit alone.
    if want != editing && (want || editing) {
        vector::roto::set_editing_layer(want.then_some(layer));
        let st = s.active_mut().ok_or(EngineError::NoDocument)?;
        let clean = st.saved_revision == st.revision;
        st.revision += 1;
        if clean {
            st.saved_revision = st.revision;
        }
        // The composite changes (a mask stops or starts applying): everything may differ on screen.
        st.last_damage = None;
    }
    Ok(json!({ "layer": layer.0, "editing": vector::roto::is_editing(layer) }))
}

// ---------------------------------------------------------------------------
// Nuke exchange
// ---------------------------------------------------------------------------

fn count_shapes(g: &Group) -> usize {
    g.children
        .iter()
        .map(|n| match n {
            Node::Shape(_) => 1,
            Node::Group(c) => count_shapes(c),
        })
        .sum()
}

/// Returns the mask as Nuke script text (the UI puts it on the clipboard or in a file).
fn export_nuke(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_id(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let area = d.doc.bounds();
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let m = l.roto_mask.as_ref().ok_or_else(|| EngineError::Other("layer has no roto mask".into()))?;
    Ok(json!({ "text": nuke::write_nk(m, f64::from(area.width()), f64::from(area.height())), "shapes": count_shapes(&m.root) }))
}

/// Reads the shapes of a Nuke Roto node from script text, replacing or appending to the mask
/// (creating the mask when the layer has none).
fn import_nuke(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "roto.import_nuke";
    let text = p.get("text").and_then(Value::as_str).ok_or_else(|| bad(CMD, "`text` must be the Nuke script text"))?;
    let append = match p.get("mode").and_then(Value::as_str) {
        None | Some("replace") => false,
        Some("append") => true,
        Some(m) => return Err(bad(CMD, format!("unknown mode `{m}` (replace, append)"))),
    };
    let id = layer_id(s, p)?;
    let height = f64::from(s.active().ok_or(EngineError::NoDocument)?.doc.bounds().height());
    let (imported, warnings) = nuke::parse_nk(text, height).map_err(|e| bad(CMD, e.to_string()))?;
    let shapes = count_shapes(&imported.root);
    s.edit("Import Nuke Roto", |doc, _| {
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if l.locks.all {
            return Err(EngineError::Other(format!("layer \"{}\" is locked", l.name)));
        }
        let m = l.roto_mask.get_or_insert_with(RotoMask::default);
        if append {
            m.append(&imported);
        } else {
            m.root.children = imported.root.children.clone();
        }
        m.validate().map_err(|e| rerr(CMD, e))?;
        Ok(())
    })?;
    Ok(json!({ "shapes": shapes, "warnings": warnings }))
}

// ---------------------------------------------------------------------------
// Specs
// ---------------------------------------------------------------------------

macro_rules! spec {
    ($id:literal, $label:literal, [$($m:literal),*], $params:expr, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[$($m),*], shortcut: None, params: $params, enabled: $en, run: $run, journal: true }
    };
}

const COMMON: &str = r##""parent":groupId?=root,"index":n? (insert position),"name":str?"##;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "view.rotoEdit",
            label: "Edit Roto Mask View",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?,"on":bool? (default: toggle)} → {layer,editing}. While on, the layer's roto mask is not applied to its pixels (the editor overlays it instead); view state, not an undo step, not saved"##,
            enabled: has_layer,
            run: view_edit,
            journal: false,
        },
        spec!("layer.roto.add", "Add Roto Mask", ["Layer", "Roto Mask"], r##"{"layer":id?}"##, has_layer, roto_add),
        spec!("layer.roto.delete", "Delete Roto Mask", ["Layer", "Roto Mask"], r##"{"layer":id?}"##, has_roto, roto_remove),
        CommandSpec {
            id: "layer.roto.info",
            label: "Roto Mask Info",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?} → {enabled,linked,density,invert,overlap,backend,transform,nodes:[shape|group…]}"##,
            enabled: has_layer,
            run: roto_info,
            journal: false,
        },
        spec!(
            "layer.rasterize.rotoMask",
            "Rasterize Roto Mask",
            ["Layer", "Rasterize"],
            r##"{"layer":id?} (multiplies the roto mask into the pixel mask and removes the roto mask)"##,
            has_roto,
            roto_rasterize
        ),
        spec!(
            "roto.instance.set",
            "Roto Mask Settings",
            [],
            r##"{"layer":id?,"enabled":bool?,"linked":bool?,"density":0..1?,"invert":bool?,"overlap":"max|sum|over"?,"backend":"auto|cpu|gpu"?} → layer.roto.info"##,
            has_roto,
            instance_set
        ),
        CommandSpec {
            id: "roto.export_nuke",
            label: "Export to Nuke",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?} → {text,shapes} (a .nk fragment with one Roto node; the UI copies it to the clipboard or saves it)"##,
            enabled: has_roto,
            run: export_nuke,
            journal: false,
        },
        spec!(
            "roto.import_nuke",
            "Import from Nuke",
            [],
            r##"{"layer":id?,"text":str (a .nk file, or a copied Roto node),"mode":"replace|append"="replace"} → {shapes,warnings:[str]}; creates the roto mask if the layer has none. Static shapes only: animation, strokes and unconfirmed attributes are skipped and listed in warnings"##,
            has_layer,
            import_nuke
        ),
        spec!(
            "roto.node.add_shape",
            "Add Roto Shape",
            [],
            r##"{"layer":id?,"points":[point…],"closed":bool=true,"parent":groupId?,"index":n?,"name":str?} → {id,points:[ids]}. point: [x,y] | {"pos":[x,y],"in":[dx,dy],"out":[dx,dy],"feather":[dx,dy],"featherIn":[dx,dy],"featherOut":[dx,dy],"smooth":bool}"##,
            has_roto,
            add_shape
        ),
        spec!(
            "roto.node.add_rect",
            "Add Roto Rectangle",
            [],
            leak(format!(r##"{{"layer":id?,"rect":[x,y,w,h],{COMMON}}} → {{id,points}}"##)),
            has_roto,
            add_rect
        ),
        spec!(
            "roto.node.add_ellipse",
            "Add Roto Ellipse",
            [],
            leak(format!(r##"{{"layer":id?,"rect":[x,y,w,h],{COMMON}}} → {{id,points}}"##)),
            has_roto,
            add_ellipse
        ),
        spec!(
            "roto.node.add_freehand",
            "Add Roto Freehand Shape",
            [],
            leak(format!(
                r##"{{"layer":id?,"points":[[x,y]…] (the stroke),"tolerance":px=2,"closed":bool=true,{COMMON}}} → {{id,points}} (fitted to bezier)"##
            )),
            has_roto,
            add_freehand
        ),
        spec!("roto.node.delete", "Delete Roto Nodes", [], r##"{"layer":id?,"ids":[nodeId…]}"##, has_roto, node_delete),
        spec!("roto.node.rename", "Rename Roto Node", [], r##"{"layer":id?,"id":nodeId,"name":str}"##, has_roto, node_rename),
        spec!("roto.node.reorder", "Reorder Roto Node", [], r##"{"layer":id?,"id":nodeId,"index":n} (0 = bottom)"##, has_roto, node_reorder),
        spec!("roto.node.group", "Group Roto Nodes", [], r##"{"layer":id?,"ids":[nodeId…] (siblings),"name":str="Group"} → {id}"##, has_roto, node_group),
        spec!(
            "roto.node.ungroup",
            "Ungroup Roto Nodes",
            [],
            r##"{"layer":id?,"id":groupId} → {ids} (refused unless the group's transform, opacity, blend and visibility are neutral)"##,
            has_roto,
            node_ungroup
        ),
        spec!("roto.node.duplicate", "Duplicate Roto Nodes", [], r##"{"layer":id?,"ids":[nodeId…]} → {ids}"##, has_roto, node_duplicate),
        spec!(
            "roto.node.set",
            "Roto Node Settings",
            [],
            r##"{"layer":id?,"id":nodeId,"visible":bool?,"locked":bool?,"opacity":0..1?,"blendOp":"union|subtract|intersect|max|min|multiply|difference"?,"invert":bool? (shapes),"closed":bool? (shapes: join or open the last and first points),"color":[r,g,b]|null? (shapes: the spline's editor colour, 0-255; its feather points use it at 80% value; null = default),"blur":px? (shapes),"falloff":"linear|smooth|easeIn|easeOut"? (shapes)}"##,
            has_roto,
            node_set
        ),
        spec!(
            "roto.selection.make",
            "Selection from Roto Mask",
            [],
            r##"{"layer":id?,"node":id|0? (0/omitted = all splines together; a group or a shape id = just that),"each":bool=false (evaluate every spline on its own and union them, ignoring blend ops),"mode":"replace|add|subtract|intersect"} → {selected,shapes}"##,
            has_roto,
            make_selection
        ),
        spec!(
            "roto.selection.to_shapes",
            "Roto Shapes from Selection",
            [],
            r##"{"layer":id?,"tolerance":px=2 (0.5..10),"name":str="Selection"} → {shapes:[id…],group:id|null}. Adds bezier shapes following the selection's outline to the layer's roto mask (creating it if needed); holes become Subtract shapes"##,
            has_selection_and_layer,
            selection_to_shapes
        ),
        spec!(
            "roto.export_path",
            "Roto to Path",
            [],
            r##"{"layer":id?,"node":id? (a group or shape; default the whole mask),"name":str? (save in the Paths panel under this name; default the work path)} → {subpaths,name}. One subpath per visible shape with transforms applied; Subtract/Intersect/Difference blend ops carry over"##,
            has_roto,
            export_path_cmd
        ),
        spec!(
            "roto.node.transform",
            "Transform Roto Node",
            [],
            r##"{"layer":id?,"id":nodeId|0 (0 = whole mask),"translate":[x,y]?,"rotate":deg?,"scale":[sx,sy]?,"skew":deg?,"pivot":[x,y]?} (sets the node's non-destructive transform)"##,
            has_roto,
            node_transform
        ),
        spec!(
            "roto.point.add",
            "Add Roto Point",
            [],
            r##"{"layer":id?,"shape":nodeId,"pos":[x,y],"index":n?,"smooth":bool=false} → {id,index}"##,
            has_roto,
            point_add
        ),
        spec!("roto.point.delete", "Delete Roto Points", [], r##"{"layer":id?,"shape":nodeId,"ids":[pointId…]}"##, has_roto, point_delete),
        spec!(
            "roto.point.move",
            "Move Roto Points",
            [],
            r##"{"layer":id?,"shape":nodeId,"moves":[{"id":pointId,"pos":[x,y]}…]} (absolute; handles and feather move with the point)"##,
            has_roto,
            point_move
        ),
        spec!(
            "roto.point.set",
            "Edit Roto Point",
            [],
            r##"{"layer":id?,"shape":nodeId,"id":pointId,"smooth":bool?,"in":[dx,dy]?,"out":[dx,dy]?,"feather":[dx,dy]?,"featherIn":[dx,dy]?,"featherOut":[dx,dy]?}"##,
            has_roto,
            point_set
        ),
        spec!(
            "roto.point.transform",
            "Transform Roto Points",
            [],
            r##"{"layer":id?,"shape":nodeId,"ids":[pointId…]? (default all),"translate":[x,y]?,"rotate":deg?,"scale":[sx,sy]?,"skew":deg?,"pivot":[x,y]?} (relative, baked into the points)"##,
            has_roto,
            point_transform
        ),
        spec!(
            "roto.point.cusp",
            "Cusp Roto Points",
            [],
            r##"{"layer":id?,"shape":nodeId,"ids":[pointId…]? (default all)} → {changed}. Turns bezier points into square points: the handles are retracted into the point"##,
            has_roto,
            point_cusp
        ),
        spec!(
            "roto.point.smooth",
            "Smooth Roto Points",
            [],
            r##"{"layer":id?,"shape":nodeId,"ids":[pointId…]? (default all)} → {changed}. Gives each point bezier handles that average the directions of the points around it (a square point becomes a bezier point); flat at peaks and valleys, no overshoot. Works on one point or many"##,
            has_roto,
            point_smooth
        ),
        spec!(
            "roto.feather.set_point",
            "Feather Roto Point",
            [],
            r##"{"layer":id?,"shape":nodeId,"id":pointId,"offset":[dx,dy]}"##,
            has_roto,
            feather_set_point
        ),
        spec!(
            "roto.feather.scale_selected",
            "Scale Roto Feather",
            [],
            r##"{"layer":id?,"shape":nodeId,"ids":[pointId…]? (default all),"factor":n} (1 = no change, 0 = remove the feather)"##,
            has_roto,
            feather_scale_selected
        ),
        spec!(
            "roto.feather.set_all",
            "Feather All Roto Points",
            [],
            r##"{"layer":id?,"distance":px (negative = inward, 0 clears),"ids":[nodeId…]? (shapes or groups; default the whole mask)} → {shapes}"##,
            has_roto,
            feather_set_all
        ),
    ]
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[cfg(test)]
mod tests;
