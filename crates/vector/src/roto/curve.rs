use photocraft_doc::roto::{MAX_FEATHER, Point as RotoPoint, Shape, Transform2D, V2};
use photocraft_doc::{Knot, Path, Subpath};
use photocraft_geom::Point;

/// Applies translate/rotate/scale/skew about the pivot.
pub(crate) fn transform_point(t: &Transform2D, p: V2) -> V2 {
    let (dx, dy) = (p.x - t.pivot.x, p.y - t.pivot.y);
    let (sx, sy) = (dx * t.scale.x, dy * t.scale.y);
    let skew = t.skew.to_radians().tan();
    let (kx, ky) = (sx + sy * skew, sy);
    let (sin, cos) = t.rotate.to_radians().sin_cos();
    V2::new(t.pivot.x + t.translate.x + kx * cos - ky * sin, t.pivot.y + t.translate.y + kx * sin + ky * cos)
}

fn apply(chain: &[Transform2D], p: V2) -> Point {
    let q = chain.iter().fold(p, |acc, t| transform_point(t, acc));
    Point::new(q.x, q.y)
}

fn knot(p: &RotoPoint, chain: &[Transform2D], feather: bool) -> Knot {
    let (base, tin, tout) = if feather {
        let off = V2::new(p.feather_pos.x.clamp(-MAX_FEATHER, MAX_FEATHER), p.feather_pos.y.clamp(-MAX_FEATHER, MAX_FEATHER));
        (V2::new(p.pos.x + off.x, p.pos.y + off.y), p.feather_in, p.feather_out)
    } else {
        (p.pos, p.tangent_in, p.tangent_out)
    };
    Knot {
        anchor: apply(chain, base),
        in_ctrl: apply(chain, V2::new(base.x + tin.x, base.y + tin.y)),
        out_ctrl: apply(chain, V2::new(base.x + tout.x, base.y + tout.y)),
        smooth: p.smooth,
    }
}

/// The shape outline (`feather == false`) or its feather outline as a path. `chain` applies in
/// order: the shape's own transform first, then each enclosing group's, innermost to outermost.
pub(crate) fn outline_path(s: &Shape, chain: &[Transform2D], feather: bool) -> Path {
    let mut path = Path::default();
    if s.points.len() < 2 {
        return path;
    }
    path.subpaths.push(Subpath { knots: knots(s, chain, feather), closed: s.closed, ..Default::default() });
    path
}

/// The transformed knots of the shape outline or feather outline.
pub(crate) fn knots(s: &Shape, chain: &[Transform2D], feather: bool) -> Vec<Knot> {
    s.points.iter().map(|p| knot(p, chain, feather)).collect()
}
