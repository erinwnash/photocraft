//! Roto tool editing logic with no egui types: where things are on the canvas, what a click or a
//! drag hit, and which engine command a gesture becomes. The overlay and tool glue live in
//! `roto_ui.rs`; every edit goes through the `roto.*` commands.
//!
//! Handles are drawn and hit-tested at their *document* positions, which include the shape's own
//! transform, its groups' and the mask's (a linked layer move rides on the root transform).
//! Gestures are mapped back to the shape's local space with the inverse of that chain before they
//! are sent, so editing a transformed shape feels the same as editing an untransformed one.

use std::collections::HashSet;

use photocraft_doc::RotoMask;
use photocraft_doc::roto::{Group, Node, NodeId, PointId, Shape, Transform2D, V2};
use photocraft_geom::Affine;
use serde_json::{Value, json};

pub type P2 = [f64; 2];

/// Which points are selected, all in one shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub shape: Option<NodeId>,
    pub points: Vec<PointId>,
}

impl Selection {
    pub fn clear(&mut self) {
        *self = Selection::default();
    }

    pub fn contains(&self, shape: NodeId, point: PointId) -> bool {
        self.shape == Some(shape) && self.points.contains(&point)
    }

    /// Selects only `point`, or toggles it into the selection when `additive` (⇧-click).
    pub fn click(&mut self, shape: NodeId, point: PointId, additive: bool) {
        if !additive || self.shape != Some(shape) {
            self.shape = Some(shape);
            self.points = vec![point];
            return;
        }
        match self.points.iter().position(|p| *p == point) {
            Some(i) => {
                self.points.remove(i);
                if self.points.is_empty() {
                    self.shape = None;
                }
            }
            None => self.points.push(point),
        }
    }

    /// Replaces (or with `additive`, extends) the selection with `points` of `shape`.
    pub fn set(&mut self, shape: NodeId, points: Vec<PointId>, additive: bool) {
        if additive && self.shape == Some(shape) {
            for p in points {
                if !self.points.contains(&p) {
                    self.points.push(p);
                }
            }
        } else {
            self.shape = if points.is_empty() { None } else { Some(shape) };
            self.points = points;
        }
    }

    /// Drops points that no longer exist (after a delete, undo or an import).
    pub fn prune(&mut self, mask: &RotoMask) {
        let Some(id) = self.shape else { return };
        match mask.find(id) {
            Some(Node::Shape(s)) => {
                let alive: HashSet<PointId> = s.points.iter().map(|p| p.id).collect();
                self.points.retain(|p| alive.contains(p));
                if self.points.is_empty() {
                    self.shape = None;
                }
            }
            _ => self.clear(),
        }
    }
}

/// What lies under a pointer position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    None,
    Point {
        shape: NodeId,
        point: PointId,
    },
    TangentIn {
        shape: NodeId,
        point: PointId,
    },
    TangentOut {
        shape: NodeId,
        point: PointId,
    },
    Feather {
        shape: NodeId,
        point: PointId,
    },
    /// A handle of the feather point's own bezier.
    FeatherIn {
        shape: NodeId,
        point: PointId,
    },
    FeatherOut {
        shape: NodeId,
        point: PointId,
    },
    /// On the outline between point `index` and the next, at `pos` (document px).
    Segment {
        shape: NodeId,
        index: usize,
        pos: P2,
    },
}

/// A point as drawn: everything in document space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointView {
    pub id: PointId,
    pub pos: P2,
    pub tangent_in: P2,
    pub tangent_out: P2,
    /// The feather handle, when the point has a feather offset.
    pub feather: Option<P2>,
    /// The tips of the feather point's own bezier handles (its point's handles plus the feather
    /// point's changes); the feather point itself when it has none, or the point has no feather.
    pub feather_in: P2,
    pub feather_out: P2,
}

/// One shape as drawn.
#[derive(Clone, Debug)]
pub struct ShapeView {
    pub id: NodeId,
    pub closed: bool,
    pub locked: bool,
    pub visible: bool,
    pub affine: Affine,
    pub points: Vec<PointView>,
    /// The shape's editor colours (sRGB); `None` is the default.
    pub point_color: Option<[u8; 3]>,
    pub spline_color: Option<[u8; 3]>,
    pub feather_color: Option<[u8; 3]>,
}

fn apply_chain(chain: &[Transform2D], p: V2) -> V2 {
    chain.iter().fold(p, |acc, t| t.apply(acc))
}

/// The chain's effect as one affine map, from the images of the unit basis.
fn affine_of(chain: &[Transform2D]) -> Affine {
    let o = apply_chain(chain, V2::ZERO);
    let x = apply_chain(chain, V2::new(1.0, 0.0));
    let y = apply_chain(chain, V2::new(0.0, 1.0));
    Affine { m: [x.x - o.x, x.y - o.y, y.x - o.x, y.y - o.y, o.x, o.y] }
}

fn map(a: &Affine, p: P2) -> P2 {
    let q = a.apply(photocraft_geom::Point::new(p[0], p[1]));
    [q.x, q.y]
}

fn map_vec(a: &Affine, v: P2) -> P2 {
    let [m0, m1, m2, m3, _, _] = a.m;
    [m0 * v[0] + m2 * v[1], m1 * v[0] + m3 * v[1]]
}

/// Document position to the shape's local space (`None` for a degenerate transform).
pub fn to_local(a: &Affine, p: P2) -> Option<P2> {
    a.inverse().map(|i| map(&i, p))
}

/// A document-space vector to a local-space vector.
pub fn vec_to_local(a: &Affine, v: P2) -> Option<P2> {
    a.inverse().map(|i| map_vec(&i, v))
}

fn view_of(shape: &Shape, enclosing: &[Transform2D]) -> ShapeView {
    // The shape's own transform applies first, then each enclosing group's, innermost first.
    let mut chain = vec![shape.transform];
    chain.extend(enclosing.iter().rev().copied());
    let a = affine_of(&chain);
    let points = shape
        .points
        .iter()
        .map(|p| {
            let at = |v: V2| map(&a, [v.x, v.y]);
            let pos = at(p.pos);
            let tip = |t: V2| at(V2::new(p.pos.x + t.x, p.pos.y + t.y));
            let has_feather = p.feather_pos.x != 0.0 || p.feather_pos.y != 0.0;
            let base = V2::new(p.pos.x + p.feather_pos.x, p.pos.y + p.feather_pos.y);
            let feather_tip = |t: V2, f: V2| at(V2::new(base.x + t.x + f.x, base.y + t.y + f.y));
            PointView {
                id: p.id,
                pos,
                tangent_in: tip(p.tangent_in),
                tangent_out: tip(p.tangent_out),
                feather: has_feather.then(|| at(base)),
                feather_in: feather_tip(p.tangent_in, p.feather_in),
                feather_out: feather_tip(p.tangent_out, p.feather_out),
            }
        })
        .collect();
    ShapeView {
        id: shape.id,
        closed: shape.closed,
        locked: shape.locked,
        visible: shape.visible,
        affine: a,
        points,
        point_color: shape.point_color,
        spline_color: shape.spline_color,
        feather_color: shape.feather_color,
    }
}

fn collect_views(g: &Group, enclosing: &mut Vec<Transform2D>, out: &mut Vec<ShapeView>) {
    enclosing.push(g.transform);
    for n in &g.children {
        match n {
            Node::Shape(s) => out.push(view_of(s, enclosing)),
            Node::Group(c) => {
                if c.visible {
                    collect_views(c, enclosing, out);
                }
            }
        }
    }
    enclosing.pop();
}

/// Every shape as drawn, bottom to top (the same order the mask composites in). Shapes inside
/// hidden groups are left out; hidden and locked shapes are included, flagged, so the overlay can
/// dim them.
pub fn shape_views(mask: &RotoMask) -> Vec<ShapeView> {
    let mut out = Vec::new();
    collect_views(&mask.root, &mut Vec::new(), &mut out);
    out
}

/// The view of one shape.
pub fn view_of_shape(mask: &RotoMask, id: NodeId) -> Option<ShapeView> {
    shape_views(mask).into_iter().find(|v| v.id == id)
}

/// The transform a new top-level shape gets its coordinates through (the mask's own).
pub fn root_affine(mask: &RotoMask) -> Affine {
    affine_of(&[mask.root.transform])
}

fn dist(a: P2, b: P2) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn cubic(p0: P2, c0: P2, c1: P2, p1: P2, t: f64) -> P2 {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [a * p0[0] + b * c0[0] + c * c1[0] + d * p1[0], a * p0[1] + b * c0[1] + c * c1[1] + d * p1[1]]
}

/// The outline sampled as `(segment index, point)` pairs, for drawing and hit testing.
pub fn outline_samples(v: &ShapeView, per_segment: usize) -> Vec<(usize, P2)> {
    let n = v.points.len();
    let segs = if v.closed { n } else { n.saturating_sub(1) };
    let mut out = Vec::with_capacity(segs * per_segment + 1);
    for i in 0..segs {
        let (Some(a), Some(b)) = (v.points.get(i), v.points.get((i + 1) % n)) else { continue };
        for k in 0..per_segment {
            out.push((i, cubic(a.pos, a.tangent_out, b.tangent_in, b.pos, k as f64 / per_segment as f64)));
        }
    }
    if !v.closed
        && let Some(last) = v.points.last()
    {
        out.push((segs.saturating_sub(1), last.pos));
    }
    out
}

/// The feather outline as drawn: each point's feather handle (its own position when it has none)
/// joined by the shape's curve parameters. Used for the dashed feather line.
pub fn feather_polyline(v: &ShapeView) -> Vec<P2> {
    v.points.iter().map(|p| p.feather.unwrap_or(p.pos)).collect()
}

/// What is under `at`, searching the topmost shape first. Within a shape the selected points'
/// handles win over anchors, feather handles over tangents, anchors over outline segments, so a
/// handle sitting on its own anchor can still be picked. `radius` is in document px.
/// The closest candidate within `radius`.
struct Nearest {
    radius: f64,
    best: Option<(f64, Hit)>,
}

impl Nearest {
    fn consider(&mut self, d: f64, h: Hit) {
        if d <= self.radius && self.best.is_none_or(|(bd, _)| d < bd) {
            self.best = Some((d, h));
        }
    }
}

pub fn hit_test(views: &[ShapeView], sel: &Selection, at: P2, radius: f64) -> Hit {
    for v in views.iter().rev() {
        if v.locked || !v.visible {
            continue;
        }
        let selected_here = sel.shape == Some(v.id);
        let mut b = Nearest { radius, best: None };
        // Handles of the selected points first: they take priority over any anchor.
        if selected_here {
            for p in v.points.iter().filter(|p| sel.points.contains(&p.id)) {
                if let Some(f) = p.feather {
                    b.consider(dist(at, f), Hit::Feather { shape: v.id, point: p.id });
                    if dist(p.feather_in, f) > 1e-9 {
                        b.consider(dist(at, p.feather_in), Hit::FeatherIn { shape: v.id, point: p.id });
                    }
                    if dist(p.feather_out, f) > 1e-9 {
                        b.consider(dist(at, p.feather_out), Hit::FeatherOut { shape: v.id, point: p.id });
                    }
                }
                if dist(p.tangent_in, p.pos) > 1e-9 {
                    b.consider(dist(at, p.tangent_in), Hit::TangentIn { shape: v.id, point: p.id });
                }
                if dist(p.tangent_out, p.pos) > 1e-9 {
                    b.consider(dist(at, p.tangent_out), Hit::TangentOut { shape: v.id, point: p.id });
                }
            }
        }
        if let Some((_, h)) = b.best {
            return h;
        }
        for p in &v.points {
            b.consider(dist(at, p.pos), Hit::Point { shape: v.id, point: p.id });
        }
        if let Some((_, h)) = b.best {
            return h;
        }
        for (i, q) in outline_samples(v, 32) {
            b.consider(dist(at, q), Hit::Segment { shape: v.id, index: i, pos: q });
        }
        if let Some((_, h)) = b.best {
            return h;
        }
    }
    Hit::None
}

/// The points of `shape` whose anchors lie inside the rectangle (document px, any corner order).
pub fn points_in_rect(views: &[ShapeView], shape: NodeId, a: P2, b: P2) -> Vec<PointId> {
    let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
    let (y0, y1) = (a[1].min(b[1]), a[1].max(b[1]));
    views
        .iter()
        .find(|v| v.id == shape && !v.locked)
        .map(|v| v.points.iter().filter(|p| (x0..=x1).contains(&p.pos[0]) && (y0..=y1).contains(&p.pos[1])).map(|p| p.id).collect())
        .unwrap_or_default()
}

/// The shape whose points lie in the rectangle most (the marquee's target), topmost on a tie.
pub fn marquee_target(views: &[ShapeView], a: P2, b: P2) -> Option<NodeId> {
    views
        .iter()
        .rev()
        .filter(|v| !v.locked && v.visible)
        .map(|v| (points_in_rect(views, v.id, a, b).len(), v.id))
        .filter(|(n, _)| *n > 0)
        .max_by_key(|(n, _)| *n)
        .map(|(_, id)| id)
}

// ---------------------------------------------------------------------------
// Gestures to commands
// ---------------------------------------------------------------------------

fn j(v: P2) -> Value {
    json!([v[0], v[1]])
}

/// Points as they were when a drag began, in local space.
#[derive(Clone, Debug, PartialEq)]
pub struct DragStart {
    pub shape: NodeId,
    pub affine: Affine,
    pub points: Vec<(PointId, P2)>,
}

impl DragStart {
    /// Snapshot of the selected points (or just `only` when given) of `shape`.
    pub fn capture(mask: &RotoMask, sel: &Selection, shape: NodeId, only: Option<PointId>) -> Option<DragStart> {
        let v = view_of_shape(mask, shape)?;
        let Some(Node::Shape(s)) = mask.find(shape) else { return None };
        let want: Vec<PointId> = match only {
            Some(p) => vec![p],
            None => sel.points.clone(),
        };
        let points = s.points.iter().filter(|p| want.contains(&p.id)).map(|p| (p.id, [p.pos.x, p.pos.y])).collect();
        Some(DragStart { shape, affine: v.affine, points })
    }

    /// `roto.point.move` params for a drag of `delta` document px since the start.
    pub fn move_params(&self, delta: P2, coalesce: &str) -> Option<Value> {
        let d = vec_to_local(&self.affine, delta)?;
        let moves: Vec<Value> = self.points.iter().map(|(id, p)| json!({"id": id.0, "pos": j([p[0] + d[0], p[1] + d[1]])})).collect();
        Some(json!({"shape": self.shape.0, "moves": moves, "coalesce": coalesce}))
    }
}

/// `roto.point.set` params that put a point's tangent tip at `tip` (document px).
pub fn tangent_params(shape: NodeId, point: PointId, out: bool, tip: P2, affine: &Affine, local_pos: P2, coalesce: &str) -> Option<Value> {
    let l = to_local(affine, tip)?;
    let t = [l[0] - local_pos[0], l[1] - local_pos[1]];
    let key = if out { "out" } else { "in" };
    Some(json!({"shape": shape.0, "id": point.0, key: j(t), "coalesce": coalesce}))
}

/// `roto.feather.set_point` params that put a point's feather handle at `handle` (document px).
pub fn feather_params(shape: NodeId, point: PointId, handle: P2, affine: &Affine, local_pos: P2, coalesce: &str) -> Option<Value> {
    let l = to_local(affine, handle)?;
    Some(json!({"shape": shape.0, "id": point.0, "offset": j([l[0] - local_pos[0], l[1] - local_pos[1]]), "coalesce": coalesce}))
}

/// `roto.point.set` params that put the tip of a feather handle of a point at `tip` (document px).
/// The stored value is relative to the point's own handle, so it follows that handle until edited.
pub fn feather_tangent_params(mask: &RotoMask, shape: NodeId, point: PointId, out: bool, tip: P2, affine: &Affine, coalesce: &str) -> Option<Value> {
    let Some(Node::Shape(s)) = mask.find(shape) else { return None };
    let p = s.points.iter().find(|q| q.id == point)?;
    let l = to_local(affine, tip)?;
    let own = if out { p.tangent_out } else { p.tangent_in };
    let rel = [l[0] - (p.pos.x + p.feather_pos.x + own.x), l[1] - (p.pos.y + p.feather_pos.y + own.y)];
    Some(json!({"shape": shape.0, "id": point.0, if out { "featherOut" } else { "featherIn" }: j(rel), "coalesce": coalesce}))
}

/// `roto.point.move` params that nudge the selected points by `delta` document px.
pub fn nudge_params(mask: &RotoMask, sel: &Selection, delta: P2) -> Option<Value> {
    let shape = sel.shape?;
    DragStart::capture(mask, sel, shape, None)?.move_params(delta, "roto-nudge")
}

#[cfg(test)]
mod tests;
