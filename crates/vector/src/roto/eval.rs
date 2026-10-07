use photocraft_doc::RotoMask;
use photocraft_doc::roto::{BlendOp, Group, Node, OverlapMode, Shape, Transform2D};
use photocraft_geom::Rect;

use super::blend::blend;
use super::blur::blur_plane;
use super::curve::outline_path;
use super::feather::feathered_coverage;

/// Pixels the evaluator will allocate for one call (guards hostile rects).
const MAX_PIXELS: usize = 1 << 28;

/// Coverage of `m` over `rect` (row-major, `0..=1`). An empty or oversized rect gives an empty
/// vec; an invalid mask (see [`RotoMask::validate`]) gives zeros.
pub fn roto_values(m: &RotoMask, rect: Rect) -> Vec<f32> {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let Some(n) = w.checked_mul(h).filter(|n| *n <= MAX_PIXELS) else { return Vec::new() };
    if n == 0 {
        return Vec::new();
    }
    if m.validate().is_err() {
        return vec![0.0; n];
    }
    let mut acc = vec![0.0f32; n];
    let mut xf: Vec<Transform2D> = Vec::new();
    eval_children(&m.root, rect, m.overlap, &mut xf, &mut acc);
    for v in &mut acc {
        let x = v.clamp(0.0, 1.0) * m.density;
        *v = if m.invert { 1.0 - x } else { x };
    }
    acc
}

fn eval_children(g: &Group, rect: Rect, mode: OverlapMode, xf: &mut Vec<Transform2D>, acc: &mut [f32]) {
    for node in &g.children {
        match node {
            Node::Shape(s) if s.visible => {
                let cov = shape_coverage(s, rect, xf, mode);
                combine(acc, &cov, s.blend_op, s.opacity);
            }
            Node::Group(c) if c.visible => {
                let mut iso = vec![0.0f32; acc.len()];
                xf.push(c.transform);
                eval_children(c, rect, mode, xf, &mut iso);
                xf.pop();
                combine(acc, &iso, c.blend_op, c.opacity);
            }
            _ => {}
        }
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

/// One shape's coverage over `rect`. `xf` holds the enclosing groups' transforms, outermost first.
pub(crate) fn shape_coverage(s: &Shape, rect: Rect, xf: &[Transform2D], mode: OverlapMode) -> Vec<f32> {
    let mut chain = Vec::with_capacity(xf.len() + 1);
    chain.push(s.transform);
    chain.extend(xf.iter().rev().copied());
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let mut v = if s.blur > 0.0 {
        // Compute on a rect grown by the blur's reach so the plane's edges are constant, then crop.
        let m = (s.blur * 3.0).ceil() as i32 + 2;
        let grown = Rect::new(rect.x0.saturating_sub(m), rect.y0.saturating_sub(m), rect.x1.saturating_add(m), rect.y1.saturating_add(m));
        let (gw, gh) = (grown.width() as usize, grown.height() as usize);
        if gw.checked_mul(gh).is_some_and(|n| n <= MAX_PIXELS) {
            let mut big = raw_coverage(s, grown, &chain, mode);
            blur_plane(&mut big, gw, gh, s.blur);
            let mut out = Vec::with_capacity(w * h);
            let (dx, dy) = ((rect.x0 - grown.x0) as usize, (rect.y0 - grown.y0) as usize);
            for row in 0..h {
                let start = (row + dy) * gw + dx;
                out.extend_from_slice(big.get(start..start + w).unwrap_or(&[]));
            }
            out.resize(w * h, 0.0);
            out
        } else {
            let mut plane = raw_coverage(s, rect, &chain, mode);
            blur_plane(&mut plane, w, h, s.blur);
            plane
        }
    } else {
        raw_coverage(s, rect, &chain, mode)
    };
    if s.invert {
        for x in &mut v {
            *x = 1.0 - *x;
        }
    }
    v
}
