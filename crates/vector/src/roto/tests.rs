use photocraft_doc::roto::*;
use photocraft_geom::Rect;

use super::*;

fn square(id: u64, x0: f64, y0: f64, x1: f64, y1: f64) -> Shape {
    let mut s = Shape::new(NodeId(id), "s");
    for (i, (x, y)) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].into_iter().enumerate() {
        s.points.push(Point::corner(PointId(id * 100 + i as u64), x, y));
    }
    s
}

fn mask_with(shapes: Vec<Shape>) -> RotoMask {
    let mut m = RotoMask::default();
    m.root.children = shapes.into_iter().map(Node::Shape).collect();
    m
}

fn at(v: &[f32], w: usize, x: usize, y: usize) -> f32 {
    v[y * w + x]
}

#[test]
fn hard_square_fills_interior_only() {
    let m = mask_with(vec![square(1, 10.0, 10.0, 30.0, 30.0)]);
    let v = roto_values(&m, Rect::new(0, 0, 40, 40));
    assert!((at(&v, 40, 20, 20) - 1.0).abs() < 1e-6);
    assert!(at(&v, 40, 5, 5).abs() < 1e-6);
    assert!(at(&v, 40, 35, 20).abs() < 1e-6);
}

#[test]
fn density_opacity_and_invert() {
    let mut s = square(1, 10.0, 10.0, 30.0, 30.0);
    s.opacity = 0.5;
    let mut m = mask_with(vec![s]);
    m.density = 0.5;
    let v = roto_values(&m, Rect::new(0, 0, 40, 40));
    assert!((at(&v, 40, 20, 20) - 0.25).abs() < 1e-5);
    m.invert = true;
    let v = roto_values(&m, Rect::new(0, 0, 40, 40));
    assert!((at(&v, 40, 20, 20) - 0.75).abs() < 1e-5);
    assert!((at(&v, 40, 2, 2) - 1.0).abs() < 1e-5);
}

#[test]
fn subtract_shape_cuts_a_hole() {
    let a = square(1, 0.0, 0.0, 40.0, 40.0);
    let mut b = square(2, 15.0, 15.0, 25.0, 25.0);
    b.blend_op = BlendOp::Subtract;
    let v = roto_values(&mask_with(vec![a, b]), Rect::new(0, 0, 40, 40));
    assert!(at(&v, 40, 20, 20).abs() < 1e-6);
    assert!((at(&v, 40, 5, 5) - 1.0).abs() < 1e-6);
}

#[test]
fn group_opacity_applies_to_the_whole_group() {
    let mut g = Group::new(NodeId(9), "g");
    g.opacity = 0.5;
    g.children.push(Node::Shape(square(1, 0.0, 0.0, 20.0, 20.0)));
    g.children.push(Node::Shape(square(2, 10.0, 10.0, 30.0, 30.0)));
    let mut m = RotoMask::default();
    m.root.children.push(Node::Group(g));
    let v = roto_values(&m, Rect::new(0, 0, 40, 40));
    // The overlap is 1.0 inside the group (union), then halved once, not twice.
    assert!((at(&v, 40, 15, 15) - 0.5).abs() < 1e-5);
}

#[test]
fn degenerate_inputs_do_not_panic() {
    let mut s = Shape::new(NodeId(1), "one-point");
    s.points.push(Point::corner(PointId(1), 5.0, 5.0));
    let v = roto_values(&mask_with(vec![s]), Rect::new(0, 0, 8, 8));
    assert!(v.iter().all(|x| *x == 0.0));
    let mut bad = square(2, 0.0, 0.0, 4.0, 4.0);
    bad.points[0].pos.x = f64::NAN;
    let v = roto_values(&mask_with(vec![bad]), Rect::new(0, 0, 8, 8));
    assert!(v.iter().all(|x| x.is_finite()));
    assert!(roto_values(&RotoMask::default(), Rect::EMPTY).is_empty());
}

#[test]
fn group_and_shape_transforms_chain_inner_first() {
    // Shape translates +10 in x on its own, then its group translates +5 in y.
    let mut s = square(1, 0.0, 0.0, 10.0, 10.0);
    s.transform.translate = V2::new(10.0, 0.0);
    let mut g = Group::new(NodeId(9), "g");
    g.transform.translate = V2::new(0.0, 5.0);
    g.children.push(Node::Shape(s));
    let mut m = RotoMask::default();
    m.root.children.push(Node::Group(g));
    let v = roto_values(&m, Rect::new(0, 0, 40, 40));
    assert!((at(&v, 40, 15, 10) - 1.0).abs() < 1e-6, "moved square covers (15,10)");
    assert!(at(&v, 40, 5, 5).abs() < 1e-6, "original position is empty");
    assert!(at(&v, 40, 15, 2).abs() < 1e-6, "group offset applied");
}

fn feathered_square(d: f64) -> Shape {
    let mut s = square(1, 20.0, 20.0, 60.0, 60.0);
    // Pull every point's feather handle diagonally outward by `d` px on each axis.
    let dirs = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
    for (p, (dx, dy)) in s.points.iter_mut().zip(dirs) {
        p.feather_pos = V2::new(dx * d, dy * d);
    }
    s
}

#[test]
fn outward_feather_ramps_from_one_to_zero() {
    let v = roto_values(&mask_with(vec![feathered_square(10.0)]), Rect::new(0, 0, 80, 80));
    let mid = at(&v, 80, 40, 40);
    let in_band = at(&v, 80, 40, 18); // pixel centre y=18.5: 1.5px above the edge, 10px band
    let far = at(&v, 80, 40, 2);
    assert!((mid - 1.0).abs() < 1e-5);
    assert!((in_band - 0.85).abs() < 0.02, "{in_band}");
    assert!(far.abs() < 1e-5);
}

#[test]
fn feather_on_one_point_only_is_smooth_and_bounded() {
    let mut s = square(1, 20.0, 20.0, 60.0, 60.0);
    s.points[1].feather_pos = V2::new(15.0, -15.0);
    let v = roto_values(&mask_with(vec![s]), Rect::new(0, 0, 80, 80));
    assert!(v.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)));
    assert!(at(&v, 80, 25, 62).abs() < 1e-5, "far side is untouched");
    assert!(at(&v, 80, 58, 15) > 0.0, "feathered corner has a soft edge");
}

#[test]
fn inward_feather_softens_inside_and_never_leaks_outside() {
    let mut s = feathered_square(10.0);
    for p in &mut s.points {
        p.feather_pos = V2::new(-p.feather_pos.x, -p.feather_pos.y);
    }
    let v = roto_values(&mask_with(vec![s]), Rect::new(0, 0, 80, 80));
    assert!(at(&v, 80, 40, 22) < 0.5, "2px inside the top edge is mostly transparent");
    assert!((at(&v, 80, 40, 40) - 1.0).abs() < 1e-5);
    assert!(at(&v, 80, 40, 18).abs() < 1e-5);
}

#[test]
fn falloff_presets_are_monotonic_and_pinned() {
    for f in [Falloff::Linear, Falloff::Smooth, Falloff::EaseIn, Falloff::EaseOut] {
        assert_eq!(super::feather::falloff(f, 0.0), 0.0);
        assert_eq!(super::feather::falloff(f, 1.0), 1.0);
        let mut last = 0.0;
        for i in 0..=100 {
            let y = super::feather::falloff(f, i as f32 / 100.0);
            assert!(y >= last - 1e-6);
            last = y;
        }
    }
}

#[test]
fn falloff_changes_the_band_profile() {
    let mut lin = feathered_square(10.0);
    let mut ease = feathered_square(10.0);
    lin.falloff = Falloff::Linear;
    ease.falloff = Falloff::EaseIn;
    let a = at(&roto_values(&mask_with(vec![lin]), Rect::new(0, 0, 80, 80)), 80, 40, 15);
    let b = at(&roto_values(&mask_with(vec![ease]), Rect::new(0, 0, 80, 80)), 80, 40, 15);
    assert!((a - 0.5).abs() < 0.06 && (b - 0.25).abs() < 0.06, "{a} {b}");
}

#[test]
fn blur_spreads_the_edge_and_oversized_blur_is_rejected() {
    let mut s = square(1, 20.0, 20.0, 60.0, 60.0);
    s.blur = 6.0;
    let v = roto_values(&mask_with(vec![s]), Rect::new(0, 0, 80, 80));
    let edge = at(&v, 80, 40, 20);
    assert!(edge > 0.4 && edge < 0.8, "{edge}");
    assert!(at(&v, 80, 40, 40) > 0.99);
    let mut huge = square(2, 20.0, 20.0, 60.0, 60.0);
    huge.blur = 1e9;
    let v = roto_values(&mask_with(vec![huge]), Rect::new(0, 0, 80, 80));
    assert!(v.iter().all(|x| *x == 0.0), "invalid mask is empty, and returns promptly");
}

#[test]
fn shape_invert_flips_after_feather() {
    let mut s = feathered_square(10.0);
    s.invert = true;
    let v = roto_values(&mask_with(vec![s]), Rect::new(0, 0, 80, 80));
    assert!(at(&v, 80, 40, 40).abs() < 1e-5);
    assert!((at(&v, 80, 40, 2) - 1.0).abs() < 1e-5);
}

#[test]
fn overlap_modes_differ_where_bands_fold() {
    // A 25px inward feather on a 40px square folds the band over itself.
    let mut s = feathered_square(25.0);
    for p in &mut s.points {
        p.feather_pos = V2::new(-p.feather_pos.x, -p.feather_pos.y);
    }
    let mut m = mask_with(vec![s]);
    let rect = Rect::new(0, 0, 80, 80);
    m.overlap = OverlapMode::Max;
    let max = roto_values(&m, rect);
    m.overlap = OverlapMode::Sum;
    let sum = roto_values(&m, rect);
    m.overlap = OverlapMode::Over;
    let over = roto_values(&m, rect);
    assert!(max.iter().zip(&sum).all(|(a, b)| *a <= *b + 1e-6));
    assert!(max.iter().zip(&over).all(|(a, b)| *a <= *b + 1e-6));
    assert!(over.iter().zip(&sum).all(|(a, b)| *a <= *b + 1e-6));
    assert!(max.iter().zip(&sum).any(|(a, b)| *b > *a + 0.01), "Sum must exceed Max somewhere in the fold");
    assert!(sum.iter().all(|x| (0.0..=1.0).contains(x)));
}

#[test]
fn huge_or_nonfinite_feather_stays_bounded() {
    let mut s = feathered_square(1e12);
    let v = roto_values(&mask_with(vec![s.clone()]), Rect::new(0, 0, 64, 64));
    assert!(v.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)));
    s.points[0].feather_pos.x = f64::INFINITY;
    let v = roto_values(&mask_with(vec![s]), Rect::new(0, 0, 64, 64));
    assert!(v.iter().all(|x| *x == 0.0), "non-finite input invalidates the mask");
}

#[test]
fn full_frame_with_maximum_feather_and_blur_finishes() {
    // 2048x1556 with the largest allowed feather and blur must stay bounded in memory and time.
    let mut s = feathered_square(MAX_FEATHER);
    s.blur = 40.0;
    let started = std::time::Instant::now();
    let v = roto_values(&mask_with(vec![s]), Rect::new(0, 0, 2048, 1556));
    assert_eq!(v.len(), 2048 * 1556);
    assert!(v.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)));
    assert!(started.elapsed().as_secs() < 20, "took {:?}", started.elapsed());
}

#[test]
fn root_group_transform_applies() {
    let mut m = mask_with(vec![square(1, 0.0, 0.0, 10.0, 10.0)]);
    m.root.transform.translate = V2::new(20.0, 0.0);
    let v = roto_values(&m, Rect::new(0, 0, 40, 40));
    assert!((at(&v, 40, 25, 5) - 1.0).abs() < 1e-6);
    assert!(at(&v, 40, 5, 5).abs() < 1e-6);
}

/// A circle from four smooth points with a uniform outward feather. Feather tangents are stored
/// relative to the shape's, so scaling them by `d / r` makes the feather outline a true offset
/// circle: the soft edge is then the same width at every angle, not just at the points. (Left at
/// zero the handles stay circle-sized and the outline pinches between points, as in Nuke.)
#[test]
fn feather_tangents_are_relative_and_a_scaled_feather_is_an_even_offset() {
    let (cx, cy, r, d, k) = (50.0, 50.0, 30.0, 10.0, 30.0 * 0.552_284_749_8);
    let mut s = Shape::new(NodeId(1), "circle");
    let dirs = [(0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)];
    for (i, (dx, dy)) in dirs.into_iter().enumerate() {
        let mut p = Point::corner(PointId(i as u64 + 1), cx + dx * r, cy + dy * r);
        // Clockwise tangents (y down): out is the quarter-turn direction.
        p.tangent_out = V2::new(-dy * k, dx * k);
        p.tangent_in = V2::new(dy * k, -dx * k);
        p.feather_pos = V2::new(dx * d, dy * d);
        p.feather_out = V2::new(p.tangent_out.x * d / r, p.tangent_out.y * d / r);
        p.feather_in = V2::new(p.tangent_in.x * d / r, p.tangent_in.y * d / r);
        p.smooth = true;
        s.points.push(p);
    }
    let v = roto_values(&mask_with(vec![s]), Rect::new(0, 0, 100, 100));
    // Halfway through the band (5px outside the circle) the ramp should read about 0.5 at any angle.
    for deg in [0.0f64, 20.0, 45.0, 70.0, 135.0, 200.0, 300.0] {
        let (sn, cs) = deg.to_radians().sin_cos();
        let (x, y) = (cx + cs * (r + 5.0), cy + sn * (r + 5.0));
        let a = at(&v, 100, x.floor() as usize, y.floor() as usize);
        assert!((a - 0.5).abs() < 0.1, "angle {deg}: {a}");
    }
}
