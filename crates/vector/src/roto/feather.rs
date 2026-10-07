//! Feather band: the soft edge between a shape outline and its feather outline.
//!
//! Both outlines are sampled at matched parameters, so sample *i* on one pairs with sample *i* on
//! the other. A point whose feather handle points outward ramps alpha 1 → 0 from the shape edge to
//! the feather edge; one pointing inward ramps 0 → 1 and the solid core shrinks to the feather
//! outline there. The band is rasterized as Gouraud triangles, one quad per sample pair.

use photocraft_doc::roto::{Falloff, OverlapMode, Shape, Transform2D};
use photocraft_doc::{Knot, Path, Subpath};
use photocraft_geom::Rect;

use super::curve::knots;

type P2 = (f64, f64);

/// Maps the band parameter `x` (0 at the feather edge, 1 at the shape side) through a preset.
pub(crate) fn falloff(f: Falloff, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    match f {
        Falloff::Linear => x,
        Falloff::Smooth => x * x * (3.0 - 2.0 * x),
        Falloff::EaseIn => x * x,
        Falloff::EaseOut => 1.0 - (1.0 - x) * (1.0 - x),
    }
}

/// How two overlapping contributions to the same pixel combine.
pub(crate) fn combine_overlap(mode: OverlapMode, a: f32, b: f32) -> f32 {
    match mode {
        OverlapMode::Max => a.max(b),
        OverlapMode::Sum => (a + b).min(1.0),
        OverlapMode::Over => a + b - a * b,
    }
}

/// Whether any point of `s` has a feather offset.
pub(crate) fn has_feather(s: &Shape) -> bool {
    s.points.iter().any(|p| p.feather_pos.x != 0.0 || p.feather_pos.y != 0.0)
}

fn cubic(p0: P2, c0: P2, c1: P2, p1: P2, t: f64) -> P2 {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (a * p0.0 + b * c0.0 + c * c1.0 + d * p1.0, a * p0.1 + b * c0.1 + c * c1.1 + d * p1.1)
}

fn pt(p: photocraft_geom::Point) -> P2 {
    (p.x, p.y)
}

fn ctrl_len(a: &Knot, b: &Knot) -> f64 {
    let d = |p: P2, q: P2| ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt();
    d(pt(a.anchor), pt(a.out_ctrl)) + d(pt(a.out_ctrl), pt(b.in_ctrl)) + d(pt(b.in_ctrl), pt(b.anchor))
}

/// Per point: does the feather handle point away from the polygon's centroid?
fn outward_flags(kp: &[Knot], kf: &[Knot], closed: bool) -> Vec<bool> {
    let n = kp.len();
    let (mut cx, mut cy) = (0.0, 0.0);
    for k in kp {
        cx += k.anchor.x;
        cy += k.anchor.y;
    }
    let (cx, cy) = (cx / n as f64, cy / n as f64);
    (0..n)
        .map(|k| {
            let prev = if closed { (k + n - 1) % n } else { k.saturating_sub(1) };
            let next = if closed { (k + 1) % n } else { (k + 1).min(n - 1) };
            let chord = (kp[next].anchor.x - kp[prev].anchor.x, kp[next].anchor.y - kp[prev].anchor.y);
            let mut normal = (chord.1, -chord.0);
            if normal.0 * (kp[k].anchor.x - cx) + normal.1 * (kp[k].anchor.y - cy) < 0.0 {
                normal = (-normal.0, -normal.1);
            }
            let off = (kf[k].anchor.x - kp[k].anchor.x, kf[k].anchor.y - kp[k].anchor.y);
            off.0 * normal.0 + off.1 * normal.1 >= 0.0
        })
        .collect()
}

struct Samples {
    p: Vec<P2>,
    f: Vec<P2>,
    /// Alpha at the shape-outline vertex (1 outward, 0 inward); the feather vertex gets `1 - a`.
    a: Vec<f32>,
}

fn sample(kp: &[Knot], kf: &[Knot], closed: bool, flags: &[bool]) -> Samples {
    let n = kp.len();
    let segs = if closed { n } else { n - 1 };
    let mut s = Samples { p: Vec::new(), f: Vec::new(), a: Vec::new() };
    let at = |k: usize| if flags[k] { 1.0f32 } else { 0.0 };
    for i in 0..segs {
        let j = (i + 1) % n;
        let len = ctrl_len(&kp[i], &kp[j]).max(ctrl_len(&kf[i], &kf[j]));
        let steps = ((len / 2.0).ceil() as usize).clamp(8, 256);
        for k in 0..steps {
            let t = k as f64 / steps as f64;
            s.p.push(cubic(pt(kp[i].anchor), pt(kp[i].out_ctrl), pt(kp[j].in_ctrl), pt(kp[j].anchor), t));
            s.f.push(cubic(pt(kf[i].anchor), pt(kf[i].out_ctrl), pt(kf[j].in_ctrl), pt(kf[j].anchor), t));
            s.a.push(at(i) + (at(j) - at(i)) * t as f32);
        }
    }
    if !closed {
        let (p, f) = (kp[n - 1].anchor, kf[n - 1].anchor);
        s.p.push((p.x, p.y));
        s.f.push((f.x, f.y));
        s.a.push(at(n - 1));
    }
    s
}

fn edge(a: P2, b: P2, p: P2) -> f64 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

/// Shared edges are owned by exactly one of the two triangles that meet there.
fn top_left(a: P2, b: P2) -> bool {
    b.1 < a.1 || (b.1 == a.1 && b.0 > a.0)
}

/// Scanline-rasterizes one Gouraud triangle into `plane` (row-major over `rect`).
fn triangle(plane: &mut [f32], rect: Rect, mut v: [P2; 3], mut al: [f32; 3], fall: Falloff, mode: OverlapMode) {
    if !v.iter().all(|p| p.0.is_finite() && p.1.is_finite()) {
        return;
    }
    let mut area = edge(v[0], v[1], v[2]);
    if area == 0.0 || !area.is_finite() {
        return;
    }
    if area < 0.0 {
        v.swap(1, 2);
        al.swap(1, 2);
        area = -area;
    }
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let (ox, oy) = (f64::from(rect.x0), f64::from(rect.y0));
    let min_y = v.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let max_y = v.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let row0 = (min_y - 0.5 - oy).ceil().clamp(0.0, h as f64) as usize;
    let row1 = ((max_y - 0.5 - oy).floor() + 1.0).clamp(0.0, h as f64) as usize;
    let edges = [(v[1], v[2]), (v[2], v[0]), (v[0], v[1])];
    for row in row0..row1 {
        let yc = oy + row as f64 + 0.5;
        // x interval where every edge function is >= 0: e(x) = k0 - dy * x.
        let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
        let mut empty = false;
        for (a, b) in edges {
            let dy = b.1 - a.1;
            let k0 = (b.0 - a.0) * (yc - a.1) + dy * a.0;
            if dy == 0.0 {
                empty |= k0 < 0.0;
            } else if dy > 0.0 {
                hi = hi.min(k0 / dy);
            } else {
                lo = lo.max(k0 / dy);
            }
        }
        if empty || lo > hi {
            continue;
        }
        let c0 = (lo - 0.5 - ox).ceil().clamp(0.0, w as f64) as usize;
        let c1 = ((hi - 0.5 - ox).floor() + 1.0).clamp(0.0, w as f64) as usize;
        for col in c0..c1 {
            let p = (ox + col as f64 + 0.5, yc);
            let mut e = [0.0f64; 3];
            let mut inside = true;
            for (i, (a, b)) in edges.iter().enumerate() {
                e[i] = edge(*a, *b, p);
                inside &= e[i] > 0.0 || (e[i] == 0.0 && top_left(*a, *b));
            }
            if !inside {
                continue;
            }
            let t = ((e[0] * f64::from(al[0]) + e[1] * f64::from(al[1]) + e[2] * f64::from(al[2])) / area) as f32;
            if let Some(px) = plane.get_mut(row * w + col) {
                *px = combine_overlap(mode, *px, falloff(fall, t));
            }
        }
    }
}

/// Coverage of `s` (core plus feather band) over `rect`, or `None` when the shape has no feather
/// or is too small to have a band (the caller then fills the plain outline).
pub(crate) fn feathered_coverage(s: &Shape, chain: &[Transform2D], rect: Rect, mode: OverlapMode) -> Option<Vec<f32>> {
    if s.points.len() < 2 || !has_feather(s) {
        return None;
    }
    let kp = knots(s, chain, false);
    let kf = knots(s, chain, true);
    let flags = outward_flags(&kp, &kf, s.closed);
    let sm = sample(&kp, &kf, s.closed, &flags);
    let count = sm.p.len();
    // Core: the shape outline, pulled in to the feather outline wherever the feather points inward.
    let core_pts: Vec<P2> = (0..count)
        .map(|i| {
            let a = f64::from(sm.a[i]);
            (a * sm.p[i].0 + (1.0 - a) * sm.f[i].0, a * sm.p[i].1 + (1.0 - a) * sm.f[i].1)
        })
        .collect();
    let mut core_path = Path::default();
    core_path.subpaths.push(Subpath::polygon(&core_pts));
    let mut out = crate::path_coverage(&core_path, rect);
    let mut band = vec![0.0f32; out.len()];
    let quads = if s.closed { count } else { count - 1 };
    for i in 0..quads {
        let j = (i + 1) % count;
        let (p0, p1, f1, f0) = (sm.p[i], sm.p[j], sm.f[j], sm.f[i]);
        let (a0, a1) = (sm.a[i], sm.a[j]);
        triangle(&mut band, rect, [p0, p1, f1], [a0, a1, 1.0 - a1], s.falloff, mode);
        triangle(&mut band, rect, [p0, f1, f0], [a0, 1.0 - a1, 1.0 - a0], s.falloff, mode);
    }
    for (o, b) in out.iter_mut().zip(&band) {
        *o = combine_overlap(mode, *o, *b);
    }
    Some(out)
}
