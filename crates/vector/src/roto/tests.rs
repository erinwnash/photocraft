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
