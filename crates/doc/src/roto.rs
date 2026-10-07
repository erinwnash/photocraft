//! Roto mask data: a tree of bezier shapes and groups that evaluates to an alpha mask.
//! Pure data; evaluation lives in `photocraft-roto`. Coordinates are document pixels (f64, y down).

use serde::{Deserialize, Serialize};

pub const MAX_POINTS_PER_SHAPE: usize = 10_000;
pub const MAX_DEPTH: usize = 32;
pub const MAX_NODES: usize = 10_000;
pub const MAX_BLUR: f32 = 1000.0;
pub const MAX_FEATHER: f64 = 10_000.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u64);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    fn is_finite(&self) -> bool {
        [self.pos, self.tangent_in, self.tangent_out, self.feather_pos, self.feather_in, self.feather_out].iter().all(V2::is_finite)
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
    if !g.transform.is_finite() {
        return Err(RotoError::NonFinite(g.name.clone()));
    }
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
                if !s.transform.is_finite() || !s.points.iter().all(Point::is_finite) {
                    return Err(RotoError::NonFinite(s.name.clone()));
                }
            }
        }
    }
    Ok(())
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
}
