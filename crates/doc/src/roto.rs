//! Roto mask data: a tree of bezier shapes and groups that evaluates to an alpha mask.
//! Pure data; evaluation lives in `photocraft-roto`. Coordinates are document pixels (f64, y down).

use serde::{Deserialize, Serialize};

pub const MAX_POINTS_PER_SHAPE: usize = 10_000;
pub const MAX_DEPTH: usize = 32;
pub const MAX_NODES: usize = 10_000;
pub const MAX_BLUR: f32 = 1000.0;
pub const MAX_FEATHER: f64 = 10_000.0;
/// Largest accepted point, handle, offset, translate or pivot component (document pixels).
pub const MAX_COORD: f64 = 1.0e7;
/// Largest accepted scale factor.
pub const MAX_SCALE: f64 = 1.0e6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u64);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PointId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct V2 {
    pub x: f64,
    pub y: f64,
}
impl V2 {
    pub const ZERO: V2 = V2 { x: 0.0, y: 0.0 };
    pub fn new(x: f64, y: f64) -> Self {
        V2 { x, y }
    }
    pub fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendOp {
    /// `a + b - ab` (alpha over).
    #[default]
    Union,
    /// `a * (1 - b)`.
    Subtract,
    /// `min(a, b)`.
    Intersect,
    Max,
    Min,
    /// `a * b`.
    Multiply,
    /// `|a - b|`.
    Difference,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Falloff {
    #[default]
    Linear,
    Smooth,
    EaseIn,
    EaseOut,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlapMode {
    #[default]
    Max,
    Sum,
    Over,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

/// Translate, rotate (degrees), scale and skew (degrees) about `pivot`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform2D {
    pub translate: V2,
    pub rotate: f64,
    pub scale: V2,
    pub skew: f64,
    pub pivot: V2,
}
impl Default for Transform2D {
    fn default() -> Self {
        Transform2D { translate: V2::ZERO, rotate: 0.0, scale: V2::new(1.0, 1.0), skew: 0.0, pivot: V2::ZERO }
    }
}
impl Transform2D {
    pub fn is_finite(&self) -> bool {
        self.translate.is_finite() && self.scale.is_finite() && self.pivot.is_finite() && self.rotate.is_finite() && self.skew.is_finite()
    }

    /// Finite and within the accepted ranges.
    fn check(&self, what: &str) -> Result<(), RotoError> {
        if !self.is_finite() {
            return Err(RotoError::NonFinite(what.into()));
        }
        let big = |v: V2, lim: f64| v.x.abs() > lim || v.y.abs() > lim;
        if big(self.translate, MAX_COORD) || big(self.pivot, MAX_COORD) || big(self.scale, MAX_SCALE) {
            return Err(RotoError::OutOfRange(format!("{what} transform")));
        }
        Ok(())
    }

    /// Maps a point: scale and skew about the pivot, rotate, then translate.
    pub fn apply(&self, p: V2) -> V2 {
        let (dx, dy) = (p.x - self.pivot.x, p.y - self.pivot.y);
        let (sx, sy) = (dx * self.scale.x, dy * self.scale.y);
        let skew = self.skew.to_radians().tan();
        let (kx, ky) = (sx + sy * skew, sy);
        let (sin, cos) = self.rotate.to_radians().sin_cos();
        V2::new(self.pivot.x + self.translate.x + kx * cos - ky * sin, self.pivot.y + self.translate.y + kx * sin + ky * cos)
    }

    /// Maps a vector (a handle or an offset): the linear part only, without pivot or translate.
    pub fn apply_vec(&self, v: V2) -> V2 {
        Transform2D { translate: V2::ZERO, pivot: V2::ZERO, ..*self }.apply(v)
    }
}

/// A bezier point. Handles and the feather offset are relative to `pos`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub id: PointId,
    pub pos: V2,
    pub tangent_in: V2,
    pub tangent_out: V2,
    /// Offset of the feather outline from `pos` (zero = no feather at this point).
    pub feather_pos: V2,
    /// Feather-outline tangents, relative to the shape's own `tangent_in`/`tangent_out` (zero keeps
    /// the feather outline parallel in curvature to the shape).
    pub feather_in: V2,
    pub feather_out: V2,
    pub smooth: bool,
}
impl Point {
    pub fn corner(id: PointId, x: f64, y: f64) -> Self {
        Point {
            id,
            pos: V2::new(x, y),
            tangent_in: V2::ZERO,
            tangent_out: V2::ZERO,
            feather_pos: V2::ZERO,
            feather_in: V2::ZERO,
            feather_out: V2::ZERO,
            smooth: false,
        }
    }
    fn vectors(&self) -> [V2; 6] {
        [self.pos, self.tangent_in, self.tangent_out, self.feather_pos, self.feather_in, self.feather_out]
    }
    fn is_finite(&self) -> bool {
        self.vectors().iter().all(V2::is_finite)
    }
    fn in_range(&self) -> bool {
        self.vectors().iter().all(|v| v.x.abs() <= MAX_COORD && v.y.abs() <= MAX_COORD)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub id: NodeId,
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    /// `0..=1`.
    pub opacity: f32,
    pub blend_op: BlendOp,
    pub invert: bool,
    pub closed: bool,
    pub points: Vec<Point>,
    /// Blur radius in px (`0..=MAX_BLUR`).
    pub blur: f32,
    pub falloff: Falloff,
    pub transform: Transform2D,
}
impl Shape {
    pub fn new(id: NodeId, name: &str) -> Self {
        Shape {
            id,
            name: name.into(),
            visible: true,
            locked: false,
            opacity: 1.0,
            blend_op: BlendOp::Union,
            invert: false,
            closed: true,
            points: Vec::new(),
            blur: 0.0,
            falloff: Falloff::Linear,
            transform: Transform2D::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: NodeId,
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    pub blend_op: BlendOp,
    pub transform: Transform2D,
    pub children: Vec<Node>,
}
impl Group {
    pub fn new(id: NodeId, name: &str) -> Self {
        Group {
            id,
            name: name.into(),
            visible: true,
            locked: false,
            opacity: 1.0,
            blend_op: BlendOp::Union,
            transform: Transform2D::default(),
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Group(Group),
    Shape(Shape),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RotoMask {
    pub enabled: bool,
    pub linked: bool,
    /// Final multiplier `0..=1`.
    pub density: f32,
    pub invert: bool,
    pub overlap: OverlapMode,
    pub backend: Backend,
    pub root: Group,
}
impl Default for RotoMask {
    fn default() -> Self {
        RotoMask {
            enabled: true,
            linked: true,
            density: 1.0,
            invert: false,
            overlap: OverlapMode::Max,
            backend: Backend::Auto,
            root: Group::new(NodeId(0), "Root"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RotoError {
    #[error("non-finite value in {0}")]
    NonFinite(String),
    #[error("tree deeper than {MAX_DEPTH}")]
    TooDeep,
    #[error("too many nodes (max {MAX_NODES})")]
    TooManyNodes,
    #[error("too many points in shape {0} (max {MAX_POINTS_PER_SHAPE})")]
    TooManyPoints(String),
    #[error("value out of range: {0}")]
    OutOfRange(String),
    #[error("no node with id {0}")]
    NoSuchNode(u64),
    #[error("{0}")]
    Invalid(String),
}

impl RotoMask {
    /// Checks caps and finiteness. Evaluators call this and treat an `Err` as an empty mask.
    pub fn validate(&self) -> Result<(), RotoError> {
        if !self.density.is_finite() || !(0.0..=1.0).contains(&self.density) {
            return Err(RotoError::OutOfRange("density".into()));
        }
        let mut count = 0usize;
        validate_group(&self.root, 0, &mut count)
    }
}

fn unit(v: f32, what: &str) -> Result<(), RotoError> {
    if v.is_finite() && (0.0..=1.0).contains(&v) { Ok(()) } else { Err(RotoError::OutOfRange(format!("{what} opacity"))) }
}

fn validate_group(g: &Group, depth: usize, count: &mut usize) -> Result<(), RotoError> {
    if depth > MAX_DEPTH {
        return Err(RotoError::TooDeep);
    }
    unit(g.opacity, &g.name)?;
    g.transform.check(&g.name)?;
    for n in &g.children {
        *count += 1;
        if *count > MAX_NODES {
            return Err(RotoError::TooManyNodes);
        }
        match n {
            Node::Group(c) => validate_group(c, depth + 1, count)?,
            Node::Shape(s) => {
                unit(s.opacity, &s.name)?;
                if s.points.len() > MAX_POINTS_PER_SHAPE {
                    return Err(RotoError::TooManyPoints(s.name.clone()));
                }
                if !s.blur.is_finite() || !(0.0..=MAX_BLUR).contains(&s.blur) {
                    return Err(RotoError::OutOfRange(format!("{} blur", s.name)));
                }
                s.transform.check(&s.name)?;
                if !s.points.iter().all(Point::is_finite) {
                    return Err(RotoError::NonFinite(s.name.clone()));
                }
                if !s.points.iter().all(Point::in_range) {
                    return Err(RotoError::OutOfRange(format!("{} point", s.name)));
                }
            }
        }
    }
    Ok(())
}

fn node_id(n: &Node) -> NodeId {
    match n {
        Node::Group(g) => g.id,
        Node::Shape(s) => s.id,
    }
}

fn find_in(g: &Group, id: NodeId) -> Option<&Node> {
    for n in &g.children {
        if node_id(n) == id {
            return Some(n);
        }
        if let Node::Group(c) = n
            && let Some(found) = find_in(c, id)
        {
            return Some(found);
        }
    }
    None
}

fn find_in_mut(g: &mut Group, id: NodeId) -> Option<&mut Node> {
    for n in &mut g.children {
        if node_id(n) == id {
            return Some(n);
        }
        if let Node::Group(c) = n
            && let Some(found) = find_in_mut(c, id)
        {
            return Some(found);
        }
    }
    None
}

fn locate_in(g: &Group, id: NodeId) -> Option<(NodeId, usize)> {
    for (i, n) in g.children.iter().enumerate() {
        if node_id(n) == id {
            return Some((g.id, i));
        }
        if let Node::Group(c) = n
            && let Some(found) = locate_in(c, id)
        {
            return Some(found);
        }
    }
    None
}

fn max_ids(g: &Group, nodes: &mut u64, points: &mut u64) {
    for n in &g.children {
        match n {
            Node::Group(c) => {
                *nodes = (*nodes).max(c.id.0);
                max_ids(c, nodes, points);
            }
            Node::Shape(s) => {
                *nodes = (*nodes).max(s.id.0);
                for p in &s.points {
                    *points = (*points).max(p.id.0);
                }
            }
        }
    }
}

fn take_id(counter: &mut u64) -> u64 {
    let v = *counter;
    *counter = counter.saturating_add(1);
    v
}

fn clone_fresh(n: &Node, next_node: &mut u64, next_point: &mut u64) -> Node {
    match n {
        Node::Shape(s) => {
            let mut c = s.clone();
            c.id = NodeId(take_id(next_node));
            for p in &mut c.points {
                p.id = PointId(take_id(next_point));
            }
            Node::Shape(c)
        }
        Node::Group(g) => {
            let mut c = g.clone();
            c.id = NodeId(take_id(next_node));
            c.children = g.children.iter().map(|ch| clone_fresh(ch, next_node, next_point)).collect();
            Node::Group(c)
        }
    }
}

impl RotoMask {
    /// An id above every node id in the tree (the root is id 0).
    pub fn next_node_id(&self) -> NodeId {
        let (mut nodes, mut points) = (self.root.id.0, 0);
        max_ids(&self.root, &mut nodes, &mut points);
        NodeId(nodes.saturating_add(1))
    }

    /// An id above every point id in the tree.
    pub fn next_point_id(&self) -> PointId {
        let (mut nodes, mut points) = (0, 0);
        max_ids(&self.root, &mut nodes, &mut points);
        PointId(points.saturating_add(1))
    }

    pub fn find(&self, id: NodeId) -> Option<&Node> {
        find_in(&self.root, id)
    }

    pub fn find_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        find_in_mut(&mut self.root, id)
    }

    pub fn shape_mut(&mut self, id: NodeId) -> Option<&mut Shape> {
        match self.find_mut(id) {
            Some(Node::Shape(s)) => Some(s),
            _ => None,
        }
    }

    /// A group by id; the root group is id 0.
    pub fn group_mut(&mut self, id: NodeId) -> Option<&mut Group> {
        if id == self.root.id {
            return Some(&mut self.root);
        }
        match self.find_mut(id) {
            Some(Node::Group(g)) => Some(g),
            _ => None,
        }
    }

    /// The parent group's id and the node's index within it.
    pub fn locate(&self, id: NodeId) -> Option<(NodeId, usize)> {
        locate_in(&self.root, id)
    }

    /// Inserts `node` into group `parent` at `index` (appended on top when `None` or past the end).
    pub fn insert(&mut self, parent: NodeId, index: Option<usize>, node: Node) -> Result<(), RotoError> {
        let g = self.group_mut(parent).ok_or(RotoError::NoSuchNode(parent.0))?;
        let at = index.map_or(g.children.len(), |i| i.min(g.children.len()));
        g.children.insert(at, node);
        Ok(())
    }

    /// Removes and returns a node (with its subtree).
    pub fn remove(&mut self, id: NodeId) -> Option<Node> {
        let (parent, index) = self.locate(id)?;
        let g = self.group_mut(parent)?;
        (index < g.children.len()).then(|| g.children.remove(index))
    }

    /// Moves a node to `index` within its parent (clamped).
    pub fn reorder(&mut self, id: NodeId, index: usize) -> Result<(), RotoError> {
        let (parent, from) = self.locate(id).ok_or(RotoError::NoSuchNode(id.0))?;
        let g = self.group_mut(parent).ok_or(RotoError::NoSuchNode(parent.0))?;
        let node = g.children.remove(from);
        let at = index.min(g.children.len());
        g.children.insert(at, node);
        Ok(())
    }

    /// Wraps sibling nodes in a new group placed where the topmost member was, keeping the
    /// members' tree order. Returns the new group's id.
    pub fn group(&mut self, ids: &[NodeId], name: &str) -> Result<NodeId, RotoError> {
        if ids.is_empty() {
            return Err(RotoError::Invalid("nothing to group".into()));
        }
        let mut seen = std::collections::HashSet::new();
        let mut parent = None;
        let mut indices = Vec::with_capacity(ids.len());
        for id in ids {
            if !seen.insert(*id) {
                return Err(RotoError::Invalid(format!("node {} listed twice", id.0)));
            }
            let (p, i) = self.locate(*id).ok_or(RotoError::NoSuchNode(id.0))?;
            if *parent.get_or_insert(p) != p {
                return Err(RotoError::Invalid("nodes to group must share a parent".into()));
            }
            indices.push(i);
        }
        let parent = parent.ok_or_else(|| RotoError::Invalid("nothing to group".into()))?;
        indices.sort_unstable();
        let gid = self.next_node_id();
        let g = self.group_mut(parent).ok_or(RotoError::NoSuchNode(parent.0))?;
        let mut members = Vec::with_capacity(indices.len());
        for i in indices.iter().rev() {
            members.push(g.children.remove(*i));
        }
        members.reverse();
        let mut group = Group::new(gid, name);
        group.children = members;
        let at = indices.first().copied().unwrap_or(0).min(g.children.len());
        g.children.insert(at, Node::Group(group));
        Ok(gid)
    }

    /// Replaces a group by its children. Refused unless the group is neutral (identity transform,
    /// opacity 1, union, visible), because ungrouping must not change the rendered mask.
    pub fn ungroup(&mut self, id: NodeId) -> Result<Vec<NodeId>, RotoError> {
        let Some(Node::Group(g)) = self.find(id) else {
            return Err(RotoError::Invalid(format!("node {} is not a group", id.0)));
        };
        if g.transform != Transform2D::default() || g.opacity != 1.0 || g.blend_op != BlendOp::Union || !g.visible {
            return Err(RotoError::Invalid("ungrouping would change the result: reset the group transform, opacity, blend and visibility first".into()));
        }
        let child_ids: Vec<NodeId> = g.children.iter().map(node_id).collect();
        let (parent, index) = self.locate(id).ok_or(RotoError::NoSuchNode(id.0))?;
        let pg = self.group_mut(parent).ok_or(RotoError::NoSuchNode(parent.0))?;
        let Node::Group(removed) = pg.children.remove(index) else {
            return Err(RotoError::Invalid("not a group".into()));
        };
        for (k, child) in removed.children.into_iter().enumerate() {
            pg.children.insert(index + k, child);
        }
        Ok(child_ids)
    }

    /// Copies `other`'s top-level nodes onto the top of this mask with fresh ids. Returns the new
    /// top-level ids in order. Instance settings (density, overlap, ...) are not copied.
    pub fn append(&mut self, other: &RotoMask) -> Vec<NodeId> {
        let (mut next_node, mut next_point) = (self.next_node_id().0, self.next_point_id().0);
        let copies: Vec<Node> = other.root.children.iter().map(|n| clone_fresh(n, &mut next_node, &mut next_point)).collect();
        let ids = copies.iter().map(node_id).collect();
        self.root.children.extend(copies);
        ids
    }

    /// Deep-copies nodes with fresh ids, each copy placed right above its original. Returns the
    /// copies' ids in the order given.
    pub fn duplicate(&mut self, ids: &[NodeId]) -> Result<Vec<NodeId>, RotoError> {
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            if !seen.insert(*id) {
                return Err(RotoError::Invalid(format!("node {} listed twice", id.0)));
            }
            if self.find(*id).is_none() {
                return Err(RotoError::NoSuchNode(id.0));
            }
        }
        let (mut next_node, mut next_point) = (self.next_node_id().0, self.next_point_id().0);
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let original = self.find(*id).cloned().ok_or(RotoError::NoSuchNode(id.0))?;
            let copy = clone_fresh(&original, &mut next_node, &mut next_point);
            out.push(node_id(&copy));
            let (parent, index) = self.locate(*id).ok_or(RotoError::NoSuchNode(id.0))?;
            self.insert(parent, Some(index + 1), copy)?;
        }
        Ok(out)
    }
}

fn affine_pt(a: &photocraft_geom::Affine, p: V2) -> V2 {
    let q = a.apply(photocraft_geom::Point::new(p.x, p.y));
    V2::new(q.x, q.y)
}

/// A vector (handle or offset) maps through the linear part only.
fn affine_vec(a: &photocraft_geom::Affine, v: V2) -> V2 {
    let [m0, m1, m2, m3, _, _] = a.m;
    V2::new(m0 * v.x + m2 * v.y, m1 * v.x + m3 * v.y)
}

/// Bakes the transform chain (the shape's own, then each enclosing group's, innermost first) and
/// `a` into every point, and resets all node transforms.
fn bake_group(g: &mut Group, chain: &mut Vec<Transform2D>, a: &photocraft_geom::Affine) {
    chain.push(g.transform);
    for n in &mut g.children {
        match n {
            Node::Group(c) => bake_group(c, chain, a),
            Node::Shape(s) => {
                let own = s.transform;
                let through = |p: V2| {
                    let q = chain.iter().rev().fold(own.apply(p), |acc, t| t.apply(acc));
                    affine_pt(a, q)
                };
                let through_vec = |v: V2| {
                    let w = chain.iter().rev().fold(own.apply_vec(v), |acc, t| t.apply_vec(acc));
                    affine_vec(a, w)
                };
                for p in &mut s.points {
                    p.pos = through(p.pos);
                    p.tangent_in = through_vec(p.tangent_in);
                    p.tangent_out = through_vec(p.tangent_out);
                    p.feather_pos = through_vec(p.feather_pos);
                    p.feather_in = through_vec(p.feather_in);
                    p.feather_out = through_vec(p.feather_out);
                }
                s.transform = Transform2D::default();
            }
        }
    }
    chain.pop();
    g.transform = Transform2D::default();
}

impl RotoMask {
    /// Applies a layer-level affine (a move, Free Transform or canvas change). A pure translation
    /// rides on the root group's transform; anything else is baked exactly into the points, and
    /// every node transform is reset. Blur radii and feather lengths are not rescaled.
    pub fn apply_affine(&mut self, a: &photocraft_geom::Affine) {
        let [m0, m1, m2, m3, tx, ty] = a.m;
        if m0 == 1.0 && m1 == 0.0 && m2 == 0.0 && m3 == 1.0 {
            self.root.transform.translate.x += tx;
            self.root.transform.translate.y += ty;
            return;
        }
        bake_group(&mut self.root, &mut Vec::new(), a);
    }
}

/// FNV-1a over the bytes of everything that affects how a mask renders.
struct Fnv(u64);

impl Fnv {
    fn put(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn u(&mut self, v: u64) {
        self.put(&v.to_le_bytes());
    }
    fn f(&mut self, v: f64) {
        self.u(v.to_bits());
    }
    fn b(&mut self, v: bool) {
        self.put(&[u8::from(v)]);
    }
    fn v2(&mut self, v: V2) {
        self.f(v.x);
        self.f(v.y);
    }
    fn transform(&mut self, t: &Transform2D) {
        // Destructured without `..`: a new field must be considered here.
        let Transform2D { translate, rotate, scale, skew, pivot } = t;
        self.v2(*translate);
        self.f(*rotate);
        self.v2(*scale);
        self.f(*skew);
        self.v2(*pivot);
    }
    fn point(&mut self, p: &Point) {
        let Point { id: _, pos, tangent_in, tangent_out, feather_pos, feather_in, feather_out, smooth } = p;
        for v in [pos, tangent_in, tangent_out, feather_pos, feather_in, feather_out] {
            self.v2(*v);
        }
        self.b(*smooth);
    }
    fn node(&mut self, n: &Node) {
        match n {
            Node::Shape(s) => {
                let Shape { id: _, name: _, visible, locked: _, opacity, blend_op, invert, closed, points, blur, falloff, transform } = s;
                self.put(b"S");
                self.b(*visible);
                self.f(f64::from(*opacity));
                self.u(*blend_op as u64);
                self.b(*invert);
                self.b(*closed);
                self.f(f64::from(*blur));
                self.u(*falloff as u64);
                self.transform(transform);
                self.u(points.len() as u64);
                points.iter().for_each(|p| self.point(p));
            }
            Node::Group(g) => self.group(g),
        }
    }
    fn group(&mut self, g: &Group) {
        let Group { id: _, name: _, visible, locked: _, opacity, blend_op, transform, children } = g;
        self.put(b"G");
        self.b(*visible);
        self.f(f64::from(*opacity));
        self.u(*blend_op as u64);
        self.transform(transform);
        self.u(children.len() as u64);
        children.iter().for_each(|n| self.node(n));
    }
}

impl RotoMask {
    /// A hash of everything that affects the rendered mask (geometry, feather, blur, opacity,
    /// blend, order, settings), and nothing that does not (ids, names, locks). Cheap and
    /// allocation-free, for cache keys that would otherwise format the whole tree.
    pub fn fingerprint(&self) -> u64 {
        let RotoMask { enabled, linked, density, invert, overlap, backend, root } = self;
        let mut h = Fnv(0xcbf2_9ce4_8422_2325);
        h.b(*enabled);
        h.b(*linked);
        h.f(f64::from(*density));
        h.b(*invert);
        h.u(*overlap as u64);
        h.u(*backend as u64);
        h.group(root);
        h.0
    }
}

fn len2(v: V2) -> f64 {
    v.x.hypot(v.y)
}

impl Shape {
    /// Whether `ids` (all points when `None`) selects the point.
    fn picks(ids: Option<&[PointId]>, p: &Point) -> bool {
        ids.is_none_or(|ids| ids.contains(&p.id))
    }

    /// Cusp: turns bezier points into square (corner) points: both handles are retracted into the
    /// point and the link between them is gone. Returns how many points were picked.
    pub fn cusp_points(&mut self, ids: Option<&[PointId]>) -> usize {
        let mut n = 0;
        for p in self.points.iter_mut().filter(|p| Self::picks(ids, p)) {
            p.tangent_in = V2::ZERO;
            p.tangent_out = V2::ZERO;
            p.smooth = false;
            n += 1;
        }
        n
    }

    /// Smooth: replaces the picked points' handles with ones that average the directions of the
    /// points around them, so the curve flows through each point without a kink (usually used on a
    /// group of points, or on a single point, and on a square point it turns it into a bezier point). The rule is a cubic Hermite spline's, applied to x and y separately along
    /// the chord length between points: the slope at a point is the central difference of its
    /// neighbours; at a peak or valley (the coordinate turns around) it is zero, so handles are
    /// level there and the curve does not overshoot; and it is limited (Fritsch-Carlson) so a
    /// rising run stays rising. Handles are a third of the way along each segment. The ends of an
    /// open shape use the one neighbour they have. The result depends only on the positions, so
    /// smoothing twice changes nothing. Returns how many points were picked.
    pub fn smooth_points(&mut self, ids: Option<&[PointId]>) -> usize {
        let auto = self.auto_handles();
        let mut n = 0;
        for (p, (out, inn)) in self.points.iter_mut().zip(auto) {
            if Self::picks(ids, p) {
                p.tangent_out = out;
                p.tangent_in = inn;
                p.smooth = true;
                n += 1;
            }
        }
        n
    }

    /// The (out, in) handle of every point under the smoothing rule of [`Shape::smooth_points`].
    fn auto_handles(&self) -> Vec<(V2, V2)> {
        let n = self.points.len();
        if n < 2 {
            return vec![(V2::ZERO, V2::ZERO); n];
        }
        let pos: Vec<V2> = self.points.iter().map(|p| p.pos).collect();
        // Segment i runs from point i to i + 1 (the last one wraps when the shape is closed).
        let segs = if self.closed { n } else { n - 1 };
        let at = |i: usize| pos.get(i % n).copied().unwrap_or(V2::ZERO);
        let chord: Vec<f64> = (0..segs).map(|i| len2(V2::new(at(i + 1).x - at(i).x, at(i + 1).y - at(i).y))).collect();
        let seg_len = |i: usize| chord.get(i).copied().unwrap_or(0.0);
        let slopes = |axis: fn(V2) -> f64| -> Vec<f64> {
            let delta: Vec<f64> = (0..segs).map(|i| if seg_len(i) > 1e-12 { (axis(at(i + 1)) - axis(at(i))) / seg_len(i) } else { 0.0 }).collect();
            let mut m: Vec<f64> = (0..n)
                .map(|k| {
                    let (prev, next) = if self.closed { (Some((k + n - 1) % n), Some((k + 1) % n)) } else { (k.checked_sub(1), (k + 1 < n).then_some(k + 1)) };
                    match (prev, next) {
                        (Some(a), Some(c)) => {
                            let (dp, dn) = (axis(at(k)) - axis(at(a)), axis(at(c)) - axis(at(k)));
                            let span = seg_len(a) + seg_len(k);
                            // A peak or valley is flat; otherwise the central difference.
                            if dp * dn < 0.0 || span <= 1e-12 { 0.0 } else { (axis(at(c)) - axis(at(a))) / span }
                        }
                        (None, Some(_)) => delta.get(k).copied().unwrap_or(0.0),
                        (Some(_), None) => delta.get(k.saturating_sub(1)).copied().unwrap_or(0.0),
                        (None, None) => 0.0,
                    }
                })
                .collect();
            // Fritsch-Carlson: alpha^2 + beta^2 <= 9 on every rising or falling segment.
            for (i, d) in delta.iter().enumerate() {
                let j = (i + 1) % n;
                if d.abs() <= 1e-12 {
                    continue;
                }
                let (a, b) = (m.get(i).copied().unwrap_or(0.0) / d, m.get(j).copied().unwrap_or(0.0) / d);
                let r = a * a + b * b;
                if r > 9.0 {
                    let tau = 3.0 / r.sqrt();
                    if let Some(x) = m.get_mut(i) {
                        *x = tau * a * d;
                    }
                    if let Some(x) = m.get_mut(j) {
                        *x = tau * b * d;
                    }
                }
            }
            m
        };
        let (mx, my) = (slopes(|v| v.x), slopes(|v| v.y));
        (0..n)
            .map(|k| {
                let m = V2::new(mx.get(k).copied().unwrap_or(0.0), my.get(k).copied().unwrap_or(0.0));
                // Out handle: a third of the next segment; in handle: a third of the previous one.
                let out = if k < segs { seg_len(k) / 3.0 } else { 0.0 };
                let inn = match (self.closed, k) {
                    (true, 0) => seg_len(segs - 1) / 3.0,
                    (false, 0) => 0.0,
                    _ => seg_len(k - 1) / 3.0,
                };
                (V2::new(m.x * out, m.y * out), V2::new(-m.x * inn, -m.y * inn))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Shape {
        let mut s = Shape::new(NodeId(1), "Bezier1");
        for (i, (x, y)) in [(10.0, 10.0), (30.0, 10.0), (30.0, 30.0), (10.0, 30.0)].into_iter().enumerate() {
            s.points.push(Point::corner(PointId(i as u64 + 1), x, y));
        }
        s
    }

    #[test]
    fn default_mask_is_valid_and_empty() {
        let m = RotoMask::default();
        assert!(m.validate().is_ok());
        assert_eq!(m.density, 1.0);
        assert!(m.root.children.is_empty());
    }

    #[test]
    fn rejects_nan_points_and_deep_trees() {
        let mut m = RotoMask::default();
        let mut s = square();
        s.points[0].pos.x = f64::NAN;
        m.root.children.push(Node::Shape(s));
        assert!(matches!(m.validate(), Err(RotoError::NonFinite(_))));

        let mut m = RotoMask::default();
        let mut g = Group::new(NodeId(2), "g");
        for i in 0..(MAX_DEPTH as u64 + 2) {
            let mut inner = Group::new(NodeId(10 + i), "n");
            inner.children.push(Node::Group(g));
            g = inner;
        }
        m.root.children.push(Node::Group(g));
        assert!(matches!(m.validate(), Err(RotoError::TooDeep)));
    }

    #[test]
    fn serde_round_trip() {
        let mut m = RotoMask::default();
        m.root.children.push(Node::Shape(square()));
        let j = serde_json::to_string(&m).unwrap();
        let back: RotoMask = serde_json::from_str(&j).unwrap();
        assert_eq!(m, back);
    }

    fn shape_with(id: u64, n: u64) -> Shape {
        let mut s = Shape::new(NodeId(id), &format!("Bezier{id}"));
        for i in 0..n {
            s.points.push(Point::corner(PointId(id * 100 + i), i as f64, 0.0));
        }
        s
    }

    fn mask3() -> RotoMask {
        let mut m = RotoMask::default();
        for id in 1..=3 {
            m.root.children.push(Node::Shape(shape_with(id, 3)));
        }
        m
    }

    fn ids(m: &RotoMask) -> Vec<u64> {
        m.root
            .children
            .iter()
            .map(|n| match n {
                Node::Shape(s) => s.id.0,
                Node::Group(g) => g.id.0,
            })
            .collect()
    }

    #[test]
    fn ids_are_allocated_above_every_existing_one() {
        let m = mask3();
        assert_eq!(m.next_node_id(), NodeId(4));
        assert_eq!(m.next_point_id(), PointId(303));
        assert_eq!(RotoMask::default().next_node_id(), NodeId(1));
        assert_eq!(RotoMask::default().next_point_id(), PointId(1));
    }

    #[test]
    fn find_locate_and_remove() {
        let mut m = mask3();
        assert!(matches!(m.find(NodeId(2)), Some(Node::Shape(s)) if s.id == NodeId(2)));
        assert_eq!(m.locate(NodeId(3)), Some((NodeId(0), 2)));
        assert!(m.find(NodeId(99)).is_none());
        assert!(m.remove(NodeId(2)).is_some());
        assert_eq!(ids(&m), vec![1, 3]);
        assert!(m.remove(NodeId(2)).is_none());
    }

    #[test]
    fn reorder_clamps_and_preserves_the_rest() {
        let mut m = mask3();
        m.reorder(NodeId(1), 2).unwrap();
        assert_eq!(ids(&m), vec![2, 3, 1]);
        m.reorder(NodeId(1), 99).unwrap();
        assert_eq!(ids(&m), vec![2, 3, 1]);
        m.reorder(NodeId(1), 0).unwrap();
        assert_eq!(ids(&m), vec![1, 2, 3]);
        assert_eq!(m.reorder(NodeId(42), 0), Err(RotoError::NoSuchNode(42)));
    }

    #[test]
    fn group_then_ungroup_preserves_order_and_ids() {
        let mut m = mask3();
        let g = m.group(&[NodeId(1), NodeId(3)], "Group1").unwrap();
        assert_eq!(g, NodeId(4));
        // The group sits where its topmost-listed member was; member order is kept.
        assert_eq!(ids(&m), vec![4, 2]);
        let Some(Node::Group(grp)) = m.find(g) else { panic!("group") };
        assert_eq!(grp.children.len(), 2);
        assert_eq!(m.locate(NodeId(3)), Some((g, 1)));
        let back = m.ungroup(g).unwrap();
        assert_eq!(back, vec![NodeId(1), NodeId(3)]);
        assert_eq!(ids(&m), vec![1, 3, 2]);
        m.validate().unwrap();
    }

    #[test]
    fn group_rejects_mixed_parents_empty_and_unknown() {
        let mut m = mask3();
        let g = m.group(&[NodeId(1), NodeId(2)], "g").unwrap();
        assert!(matches!(m.group(&[NodeId(3), NodeId(1)], "x"), Err(RotoError::Invalid(_))), "different parents");
        assert!(matches!(m.group(&[], "x"), Err(RotoError::Invalid(_))));
        assert_eq!(m.group(&[NodeId(77)], "x"), Err(RotoError::NoSuchNode(77)));
        assert!(matches!(m.group(&[g, g], "x"), Err(RotoError::Invalid(_))), "duplicate ids");
    }

    #[test]
    fn ungroup_refuses_when_it_would_change_the_result() {
        let mut m = mask3();
        let g = m.group(&[NodeId(1)], "g").unwrap();
        if let Some(Node::Group(grp)) = m.find_mut(g) {
            grp.opacity = 0.5;
        }
        assert!(matches!(m.ungroup(g), Err(RotoError::Invalid(_))));
        assert!(matches!(m.ungroup(NodeId(2)), Err(RotoError::Invalid(_))), "a shape is not a group");
    }

    #[test]
    fn duplicate_deep_copies_with_fresh_ids_after_the_originals() {
        let mut m = mask3();
        let g = m.group(&[NodeId(1), NodeId(2)], "g").unwrap();
        let copies = m.duplicate(&[g]).unwrap();
        assert_eq!(copies.len(), 1);
        assert_eq!(ids(&m), vec![g.0, copies[0].0, 3]);
        // Every node and point id in the tree is unique after duplication.
        fn collect(g: &Group, nodes: &mut Vec<u64>, points: &mut Vec<u64>) {
            for n in &g.children {
                match n {
                    Node::Group(c) => {
                        nodes.push(c.id.0);
                        collect(c, nodes, points);
                    }
                    Node::Shape(s) => {
                        nodes.push(s.id.0);
                        points.extend(s.points.iter().map(|p| p.id.0));
                    }
                }
            }
        }
        let (mut nodes, mut points) = (Vec::new(), Vec::new());
        collect(&m.root, &mut nodes, &mut points);
        let (n, p) = (nodes.len(), points.len());
        nodes.sort_unstable();
        nodes.dedup();
        points.sort_unstable();
        points.dedup();
        assert_eq!((nodes.len(), points.len()), (n, p));
        assert_eq!(n, 7);
    }

    #[test]
    fn rejects_absurd_coordinates_and_scales() {
        let mut m = RotoMask::default();
        let mut s = shape_with(1, 3);
        s.points[0].pos.x = 1.0e9;
        m.root.children.push(Node::Shape(s));
        assert!(matches!(m.validate(), Err(RotoError::OutOfRange(_))));
        let mut m = RotoMask::default();
        m.root.transform.translate.y = -1.0e9;
        assert!(matches!(m.validate(), Err(RotoError::OutOfRange(_))));
        let mut m = RotoMask::default();
        let mut s = shape_with(1, 3);
        s.transform.scale = V2::new(1.0e9, 1.0);
        m.root.children.push(Node::Shape(s));
        assert!(matches!(m.validate(), Err(RotoError::OutOfRange(_))));
    }

    #[test]
    fn transform_applies_about_the_pivot_and_vectors_ignore_translation() {
        let t = Transform2D { translate: V2::new(5.0, 0.0), rotate: 90.0, pivot: V2::new(10.0, 10.0), ..Default::default() };
        let p = t.apply(V2::new(20.0, 10.0));
        // Rotating (+10, 0) by 90 degrees about the pivot gives (0, +10), then the translate.
        assert!((p.x - 15.0).abs() < 1e-9 && (p.y - 20.0).abs() < 1e-9, "{p:?}");
        let v = t.apply_vec(V2::new(10.0, 0.0));
        assert!(v.x.abs() < 1e-9 && (v.y - 10.0).abs() < 1e-9, "{v:?}");
        assert_eq!(Transform2D::default().apply(V2::new(3.0, 4.0)), V2::new(3.0, 4.0));
    }

    #[test]
    fn apply_affine_translation_moves_the_root_and_general_affines_bake_exactly() {
        use photocraft_geom::Affine;
        let mut m = mask3();
        m.apply_affine(&Affine::translate(7.0, -2.0));
        assert_eq!(m.root.transform.translate, V2::new(7.0, -2.0));
        // Points are untouched by a translation: it rides on the root transform.
        let Some(Node::Shape(s)) = m.find(NodeId(1)) else { panic!("shape") };
        assert_eq!(s.points[1].pos, V2::new(1.0, 0.0));

        // A 90 degree rotation bakes into the points, handles and feather offsets, and resets
        // every transform so the tree renders the same geometry.
        let mut m = mask3();
        if let Some(sh) = m.shape_mut(NodeId(1)) {
            sh.transform.translate = V2::new(10.0, 0.0);
            sh.points[1].tangent_out = V2::new(2.0, 0.0);
            sh.points[1].feather_pos = V2::new(0.0, 3.0);
        }
        let g = m.group(&[NodeId(1)], "g").unwrap();
        if let Some(Node::Group(grp)) = m.find_mut(g) {
            grp.transform.translate = V2::new(0.0, 5.0);
        }
        m.apply_affine(&Affine::rotate(std::f64::consts::FRAC_PI_2));
        let Some(Node::Shape(s)) = m.find(NodeId(1)) else { panic!("shape") };
        // Original point 1 is (1, 0): shape translate -> (11, 0), group translate -> (11, 5),
        // rotate 90 degrees -> (-5, 11).
        let p = s.points[1].pos;
        assert!((p.x + 5.0).abs() < 1e-9 && (p.y - 11.0).abs() < 1e-9, "{p:?}");
        // The tangent (2, 0) rotates to (0, 2); the feather offset (0, 3) rotates to (-3, 0).
        let t = s.points[1].tangent_out;
        assert!(t.x.abs() < 1e-9 && (t.y - 2.0).abs() < 1e-9, "{t:?}");
        let f = s.points[1].feather_pos;
        assert!((f.x + 3.0).abs() < 1e-9 && f.y.abs() < 1e-9, "{f:?}");
        assert_eq!(s.transform, Transform2D::default());
        let Some(Node::Group(grp)) = m.find(g) else { panic!("group") };
        assert_eq!(grp.transform, Transform2D::default());
        m.validate().unwrap();
    }

    #[test]
    fn append_copies_nodes_with_fresh_ids_on_top() {
        let mut dst = mask3();
        let mut src = RotoMask::default();
        let g = Group::new(NodeId(1), "G");
        src.root.children.push(Node::Group(g));
        src.root.children.push(Node::Shape(shape_with(2, 2)));
        if let Some(Node::Group(g)) = src.find_mut(NodeId(1)) {
            g.children.push(Node::Shape(shape_with(3, 2)));
        }
        let before_src = src.clone();
        let new_ids = dst.append(&src);
        assert_eq!(new_ids.len(), 2, "one id per top-level node");
        assert_eq!(src, before_src, "the source is untouched");
        assert_eq!(ids(&dst)[..3], [1, 2, 3], "existing nodes keep their ids and order");
        assert_eq!(ids(&dst).len(), 5);
        assert!(new_ids.iter().all(|i| *i > NodeId(3)));
        // Ids stay unique across the whole tree, points included.
        assert!(dst.next_node_id() > *new_ids.iter().max().unwrap_or(&NodeId(0)));
        dst.validate().unwrap();
    }

    #[test]
    fn fingerprint_tracks_exactly_what_changes_the_rendering() {
        let base = {
            let mut m = mask3();
            if let Some(Node::Shape(s)) = m.root.children.first_mut() {
                s.points[0].feather_pos = V2::new(1.0, 2.0);
            }
            m
        };
        let fp = |m: &RotoMask| m.fingerprint();
        assert_eq!(fp(&base), fp(&base.clone()), "equal masks, equal fingerprints");
        // Things that do not change the picture do not change the fingerprint (no cache churn).
        let mut quiet = base.clone();
        if let Some(Node::Shape(s)) = quiet.root.children.first_mut() {
            s.name = "renamed".into();
            s.locked = true;
        }
        assert_eq!(fp(&base), fp(&quiet), "names and locks do not render");
        // Everything that does change it changes the fingerprint.
        type Edit = Box<dyn Fn(&mut RotoMask)>;
        let edits: Vec<(&str, Edit)> = vec![
            ("density", Box::new(|m| m.density = 0.5)),
            ("invert", Box::new(|m| m.invert = true)),
            ("enabled", Box::new(|m| m.enabled = false)),
            ("linked", Box::new(|m| m.linked = false)),
            ("overlap", Box::new(|m| m.overlap = OverlapMode::Sum)),
            ("backend", Box::new(|m| m.backend = Backend::Gpu)),
            ("root transform", Box::new(|m| m.root.transform.translate.x = 3.0)),
            ("point position", Box::new(|m| edit_shape(m, |s| s.points[1].pos.y += 0.001))),
            ("tangent", Box::new(|m| edit_shape(m, |s| s.points[1].tangent_out.x = 2.0))),
            ("feather", Box::new(|m| edit_shape(m, |s| s.points[0].feather_pos.x = 1.5))),
            ("feather tangent", Box::new(|m| edit_shape(m, |s| s.points[0].feather_in.y = 0.5))),
            ("smooth flag", Box::new(|m| edit_shape(m, |s| s.points[2].smooth = true))),
            ("point count", Box::new(|m| edit_shape(m, |s| s.points.truncate(2)))),
            ("opacity", Box::new(|m| edit_shape(m, |s| s.opacity = 0.9))),
            ("blend op", Box::new(|m| edit_shape(m, |s| s.blend_op = BlendOp::Subtract))),
            ("shape invert", Box::new(|m| edit_shape(m, |s| s.invert = true))),
            ("closed", Box::new(|m| edit_shape(m, |s| s.closed = false))),
            ("blur", Box::new(|m| edit_shape(m, |s| s.blur = 4.0))),
            ("falloff", Box::new(|m| edit_shape(m, |s| s.falloff = Falloff::Smooth))),
            ("visible", Box::new(|m| edit_shape(m, |s| s.visible = false))),
            ("shape transform", Box::new(|m| edit_shape(m, |s| s.transform.rotate = 5.0))),
            ("order", Box::new(|m| m.root.children.reverse())),
            ("grouping", Box::new(|m| drop(m.group(&[NodeId(1), NodeId(2)], "g")))),
        ];
        for (what, edit) in edits {
            let mut m = base.clone();
            edit(&mut m);
            assert_ne!(fp(&base), fp(&m), "{what} must change the fingerprint");
        }
    }

    fn edit_shape(m: &mut RotoMask, f: impl FnOnce(&mut Shape)) {
        if let Some(Node::Shape(s)) = m.root.children.first_mut() {
            f(s);
        }
    }

    fn square_shape() -> Shape {
        let mut s = Shape::new(NodeId(1), "sq");
        for (i, (x, y)) in [(0.0, 0.0), (60.0, 0.0), (60.0, 60.0), (0.0, 60.0)].into_iter().enumerate() {
            s.points.push(Point::corner(PointId(i as u64 + 1), x, y));
        }
        s
    }

    fn v(x: f64, y: f64) -> V2 {
        V2::new(x, y)
    }

    #[test]
    fn cusp_turns_bezier_points_into_square_points() {
        let mut s = square_shape();
        for p in &mut s.points {
            p.smooth = true;
            p.tangent_out = v(10.0, 0.0);
            p.tangent_in = v(-10.0, 0.0);
        }
        assert_eq!(s.cusp_points(Some(&[PointId(2), PointId(3)])), 2);
        assert_eq!(s.points.iter().map(|p| p.smooth).collect::<Vec<_>>(), vec![true, false, false, true]);
        assert_eq!(s.points.iter().map(|p| p.tangent_out == V2::ZERO && p.tangent_in == V2::ZERO).collect::<Vec<_>>(), vec![false, true, true, false]);
        assert_eq!(s.cusp_points(None), 4, "no ids means every point");
        assert!(s.points.iter().all(|p| !p.smooth && p.tangent_out == V2::ZERO && p.tangent_in == V2::ZERO));
    }

    #[test]
    fn smooth_turns_a_square_point_into_a_bezier_point_and_replaces_old_handles() {
        let mut s = square_shape();
        s.points[0].tangent_out = v(3.0, 40.0);
        s.points[0].tangent_in = v(0.0, -5.0);
        assert_eq!(s.smooth_points(Some(&[PointId(1), PointId(4)])), 2);
        let near = |a: V2, b: V2| (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9;
        assert!(near(s.points[0].tangent_out, v(10.0, -10.0)) && near(s.points[0].tangent_in, v(-10.0, 10.0)), "{:?}", s.points[0]);
        assert!(near(s.points[3].tangent_out, v(-10.0, -10.0)) && near(s.points[3].tangent_in, v(10.0, 10.0)), "{:?}", s.points[3]);
        assert!(s.points[0].smooth && s.points[3].smooth && !s.points[1].smooth);
        // Cusp then smooth round-trips a point.
        let mut t = square_shape();
        t.smooth_points(None);
        t.cusp_points(Some(&[PointId(1)]));
        assert!(!t.points[0].smooth && t.points[0].tangent_out == V2::ZERO && t.points[1].smooth);
        t.smooth_points(Some(&[PointId(1)]));
        assert!(t.points[0].smooth && t.points[0].tangent_out != V2::ZERO);
    }

    #[test]
    fn smooth_averages_the_directions_around_each_point() {
        let mut s = square_shape();
        assert_eq!(s.smooth_points(None), 4);
        // Point 0 sits between (0, 60) and (60, 0): handle = (next - prev) / 6.
        assert_eq!((s.points[0].tangent_out, s.points[0].tangent_in), (v(10.0, -10.0), v(-10.0, 10.0)));
        assert_eq!(s.points[2].tangent_out, v(-10.0, 10.0));
        assert!(s.points.iter().all(|p| p.smooth));
        // Smoothing is a function of the positions alone, so doing it again changes nothing.
        let before = s.clone();
        s.smooth_points(None);
        assert_eq!(s, before);
        // A subset: only those points are touched.
        let mut t = square_shape();
        assert_eq!(t.smooth_points(Some(&[PointId(1)])), 1);
        assert!(t.points[0].smooth && !t.points[1].smooth);
        assert_eq!(t.points[1].tangent_out, V2::ZERO);
    }

    #[test]
    fn smooth_flattens_peaks_and_valleys_and_does_not_overshoot() {
        // A zig-zag: the middle points are peaks and valleys, so their handles are level.
        let mut s = Shape::new(NodeId(1), "z");
        s.closed = false;
        for (i, (x, y)) in [(0.0, 0.0), (30.0, 40.0), (60.0, 0.0), (90.0, 40.0)].into_iter().enumerate() {
            s.points.push(Point::corner(PointId(i as u64 + 1), x, y));
        }
        s.smooth_points(None);
        for k in [1, 2] {
            assert_eq!((s.points[k].tangent_out.y, s.points[k].tangent_in.y), (0.0, 0.0), "level at point {k}");
            assert!(s.points[k].tangent_out.x > 0.0, "still heading forward");
        }
        // Handles stay inside each segment's band: no overshoot above 40 or below 0.
        for w in s.points.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            for y in [a.pos.y + a.tangent_out.y, b.pos.y + b.tangent_in.y] {
                assert!((0.0..=40.0).contains(&y), "{y}");
            }
        }
        // Smoothing a subset leaves the other points' handles alone.
        let mut t = s.clone();
        t.cusp_points(None);
        t.smooth_points(Some(&[PointId(2)]));
        assert!(t.points[1].smooth && t.points[1].tangent_out != V2::ZERO);
        assert!(!t.points[2].smooth && t.points[2].tangent_out == V2::ZERO);
    }

    #[test]
    fn smooth_on_an_open_shape_points_the_end_handles_along_the_only_neighbour() {
        let mut s = square_shape();
        s.closed = false;
        s.smooth_points(None);
        assert_eq!((s.points[0].tangent_out, s.points[0].tangent_in), (v(20.0, 0.0), V2::ZERO), "first point: toward the next, a third of the way");
        assert_eq!((s.points[3].tangent_in, s.points[3].tangent_out), (v(20.0, 0.0), V2::ZERO), "last point (0, 60): toward the previous (60, 60)");
        assert_eq!(s.points[1].tangent_out, v(10.0, 10.0), "inner points use both neighbours");
    }

    #[test]
    fn point_operations_ignore_what_does_not_exist_and_never_panic() {
        let mut empty = Shape::new(NodeId(1), "e");
        assert_eq!((empty.cusp_points(None), empty.smooth_points(None)), (0, 0));
        let mut one = Shape::new(NodeId(2), "o");
        one.points.push(Point::corner(PointId(1), 5.0, 5.0));
        assert_eq!(one.smooth_points(None), 1, "the point is marked smooth");
        assert_eq!((one.points[0].tangent_in, one.points[0].tangent_out), (V2::ZERO, V2::ZERO), "but a lone point has no neighbours to build handles from");
        let mut s = square_shape();
        assert_eq!(s.smooth_points(Some(&[PointId(99)])), 0);
        assert_eq!(s.cusp_points(Some(&[])), 0);
    }
}
