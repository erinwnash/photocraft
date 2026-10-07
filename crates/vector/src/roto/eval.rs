//! The roto evaluator: one tree walk (groups, transforms, opacity, blend ops) that drives an
//! [`Executor`]. The CPU executor here is the oracle; an accelerator (see `dispatch.rs`) supplies
//! its own executor for the per-pixel primitives, so both backends share every decision about
//! *what* to compute and differ only in *where* the arithmetic runs.

use photocraft_doc::RotoMask;
use photocraft_doc::roto::{BlendOp, Group, Node, OverlapMode, Shape, Transform2D};
use photocraft_geom::Rect;

use super::blend::blend;
use super::blur::blur_plane;
use super::curve::outline_path;
use super::feather::feathered_coverage;

/// Pixels the evaluator will allocate for one call (guards hostile rects).
pub const MAX_PIXELS: usize = 1 << 28;

/// The per-pixel primitives the tree walk needs. Planes are row-major `f32` over a [`Rect`].
pub trait Executor {
    /// An accumulator plane over the walk's rect.
    type Acc;

    /// A fresh, all-zero accumulator.
    fn new_acc(&mut self) -> Self::Acc;

    /// Combines one shape into `acc`. `raw` is the shape's coverage over `raw_rect` (its outline
    /// and feather, before blur and invert); `rect` is the walk's rect and lies inside `raw_rect`,
    /// which is grown by the blur's reach so blurred edges are not clamped. In order: blur `raw`
    /// by `blur` (px, 0 = none), crop to `rect`, invert if asked, then
    /// `acc = acc + (blend(op, acc, shape) - acc) * opacity`.
    #[allow(clippy::too_many_arguments)]
    fn combine_shape(&mut self, acc: &mut Self::Acc, raw: &[f32], raw_rect: Rect, rect: Rect, blur: f32, invert: bool, op: BlendOp, opacity: f32);

    /// `dst = dst + (blend(op, dst, src) - dst) * opacity`, consuming the group accumulator `src`.
    fn combine_acc(&mut self, dst: &mut Self::Acc, src: Self::Acc, op: BlendOp, opacity: f32);

    /// The final plane: clamp to `0..=1`, times `density`, then inverted when asked.
    fn finish(&mut self, acc: Self::Acc, density: f32, invert: bool) -> Vec<f32>;
}

/// Pixel count of `rect`, or the answer to give *instead* of evaluating: an empty or oversized
/// rect gives an empty plane, an invalid mask (see [`RotoMask::validate`]) gives zeros.
pub fn prepare(m: &RotoMask, rect: Rect) -> Result<usize, Vec<f32>> {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let Some(n) = w.checked_mul(h).filter(|n| *n <= MAX_PIXELS) else { return Err(Vec::new()) };
    if n == 0 {
        return Err(Vec::new());
    }
    if m.validate().is_err() {
        return Err(vec![0.0; n]);
    }
    Ok(n)
}

/// Evaluates `m` over `rect` with `exec`. Call [`prepare`] first and only run when it is `Ok`.
pub fn run<E: Executor>(m: &RotoMask, rect: Rect, exec: &mut E) -> Vec<f32> {
    let mut acc = exec.new_acc();
    if m.root.children.iter().any(renders) {
        // The root group's transform is the outermost one (it moves a linked mask with its layer).
        let mut xf: Vec<Transform2D> = vec![m.root.transform];
        walk(&m.root, rect, m.overlap, &mut xf, exec, &mut acc);
    } else {
        // Nothing to draw: the mask reveals everything, as a fresh vector mask does. Zero coverage
        // would hide the whole layer the moment a roto mask is added (or its last shape deleted).
        let everything = vec![1.0f32; rect.width() as usize * rect.height() as usize];
        exec.combine_shape(&mut acc, &everything, rect, rect, 0.0, false, BlendOp::Union, 1.0);
    }
    exec.finish(acc, m.density, m.invert)
}

/// Whether a node draws anything: a shape needs a visible outline (two points or more), a group
/// at least one visible node that does. Nodes that draw nothing are skipped rather than composited
/// as empty coverage, which under Multiply or Intersect would wipe the mask beneath them (a shape
/// still being drawn, or a group the user has emptied, must not blank the mask).
fn renders(n: &Node) -> bool {
    match n {
        Node::Shape(s) => s.visible && s.points.len() >= 2,
        Node::Group(g) => g.visible && g.children.iter().any(renders),
    }
}

fn walk<E: Executor>(g: &Group, rect: Rect, mode: OverlapMode, xf: &mut Vec<Transform2D>, exec: &mut E, acc: &mut E::Acc) {
    for node in g.children.iter().filter(|n| renders(n)) {
        match node {
            Node::Shape(s) => {
                let (raw, raw_rect) = shape_plane(s, rect, xf, mode);
                exec.combine_shape(acc, &raw, raw_rect, rect, s.blur, s.invert, s.blend_op, s.opacity);
            }
            Node::Group(c) => {
                let mut iso = exec.new_acc();
                xf.push(c.transform);
                walk(c, rect, mode, xf, exec, &mut iso);
                xf.pop();
                exec.combine_acc(acc, iso, c.blend_op, c.opacity);
            }
        }
    }
}

/// The CPU executor: the reference arithmetic every other backend is measured against.
pub struct CpuExecutor {
    pixels: usize,
}

impl CpuExecutor {
    pub fn new(rect: Rect) -> Self {
        CpuExecutor { pixels: rect.width() as usize * rect.height() as usize }
    }
}

fn combine(acc: &mut [f32], src: &[f32], op: BlendOp, opacity: f32) {
    for (a, b) in acc.iter_mut().zip(src) {
        let below = *a;
        let blended = blend(op, below, *b);
        // Opacity mixes the blended result with what was beneath.
        *a = below + (blended - below) * opacity;
    }
}

impl Executor for CpuExecutor {
    type Acc = Vec<f32>;

    fn new_acc(&mut self) -> Vec<f32> {
        vec![0.0; self.pixels]
    }

    fn combine_shape(&mut self, acc: &mut Vec<f32>, raw: &[f32], raw_rect: Rect, rect: Rect, blur: f32, invert: bool, op: BlendOp, opacity: f32) {
        let (w, h) = (rect.width() as usize, rect.height() as usize);
        let mut plane: Vec<f32> = if blur > 0.0 {
            let (gw, gh) = (raw_rect.width() as usize, raw_rect.height() as usize);
            let mut big = raw.to_vec();
            blur_plane(&mut big, gw, gh, blur);
            let (dx, dy) = (rect.x0.saturating_sub(raw_rect.x0).max(0) as usize, rect.y0.saturating_sub(raw_rect.y0).max(0) as usize);
            let mut out = Vec::with_capacity(w * h);
            for row in 0..h {
                let start = (row + dy) * gw + dx;
                out.extend_from_slice(big.get(start..start + w).unwrap_or(&[]));
            }
            out.resize(w * h, 0.0);
            out
        } else {
            raw.to_vec()
        };
        if invert {
            for x in &mut plane {
                *x = 1.0 - *x;
            }
        }
        combine(acc, &plane, op, opacity);
    }

    fn combine_acc(&mut self, dst: &mut Vec<f32>, src: Vec<f32>, op: BlendOp, opacity: f32) {
        combine(dst, &src, op, opacity);
    }

    fn finish(&mut self, mut acc: Vec<f32>, density: f32, invert: bool) -> Vec<f32> {
        for v in &mut acc {
            let x = v.clamp(0.0, 1.0) * density;
            *v = if invert { 1.0 - x } else { x };
        }
        acc
    }
}

/// Coverage of `m` over `rect` (row-major, `0..=1`) on the CPU. An empty or oversized rect gives
/// an empty vec; an invalid mask (see [`RotoMask::validate`]) gives zeros.
pub fn roto_values(m: &RotoMask, rect: Rect) -> Vec<f32> {
    match prepare(m, rect) {
        Err(answer) => answer,
        Ok(_) => run(m, rect, &mut CpuExecutor::new(rect)),
    }
}

/// The outline (and feather band) of `s` over exactly `rect`, before blur and invert.
fn raw_coverage(s: &Shape, rect: Rect, chain: &[Transform2D], mode: OverlapMode) -> Vec<f32> {
    if let Some(v) = feathered_coverage(s, chain, rect, mode) {
        return v;
    }
    let path = outline_path(s, chain, false);
    if path.subpaths.is_empty() {
        return vec![0.0; rect.width() as usize * rect.height() as usize];
    }
    crate::path_coverage(&path, rect)
}

/// One shape's coverage before blur and invert, and the rect it covers: `rect` itself, or `rect`
/// grown by the blur's reach (so a blur sees the shape's true surroundings) when the shape is
/// blurred and the grown plane fits. `xf` holds the enclosing groups' transforms, outermost first.
fn shape_plane(s: &Shape, rect: Rect, xf: &[Transform2D], mode: OverlapMode) -> (Vec<f32>, Rect) {
    let mut chain = Vec::with_capacity(xf.len() + 1);
    chain.push(s.transform);
    chain.extend(xf.iter().rev().copied());
    let mut area = rect;
    if s.blur > 0.0 {
        let m = (s.blur * 3.0).ceil() as i32 + 2;
        let grown = Rect::new(rect.x0.saturating_sub(m), rect.y0.saturating_sub(m), rect.x1.saturating_add(m), rect.y1.saturating_add(m));
        if (grown.width() as usize).checked_mul(grown.height() as usize).is_some_and(|n| n <= MAX_PIXELS) {
            area = grown;
        }
    }
    (raw_coverage(s, area, &chain, mode), area)
}
