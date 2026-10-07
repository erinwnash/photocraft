use photocraft_doc::roto::{Group, Node, NodeId, Point, PointId, Shape, V2};

use super::*;

fn square(id: u64, x0: f64, y0: f64, x1: f64, y1: f64) -> Shape {
    let mut s = Shape::new(NodeId(id), "s");
    for (i, (x, y)) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].into_iter().enumerate() {
        s.points.push(Point::corner(PointId(id * 10 + i as u64), x, y));
    }
    s
}

fn mask_of(shapes: Vec<Shape>) -> RotoMask {
    let mut m = RotoMask::default();
    m.root.children = shapes.into_iter().map(Node::Shape).collect();
    m
}

fn near(a: P2, b: P2) -> bool {
    dist(a, b) < 1e-6
}

#[test]
fn selection_click_toggle_set_and_prune() {
    let mut s = Selection::default();
    s.click(NodeId(1), PointId(10), false);
    assert_eq!((s.shape, s.points.clone()), (Some(NodeId(1)), vec![PointId(10)]));
    s.click(NodeId(1), PointId(11), true);
    assert_eq!(s.points, vec![PointId(10), PointId(11)]);
    s.click(NodeId(1), PointId(10), true);
    assert_eq!(s.points, vec![PointId(11)], "shift-click toggles off");
    s.click(NodeId(1), PointId(11), true);
    assert!(s.shape.is_none() && s.points.is_empty(), "an empty selection has no shape");
    s.click(NodeId(1), PointId(10), false);
    s.click(NodeId(2), PointId(20), true);
    assert_eq!((s.shape, s.points.clone()), (Some(NodeId(2)), vec![PointId(20)]), "selection never spans shapes");

    s.set(NodeId(1), vec![PointId(10), PointId(11)], false);
    s.set(NodeId(1), vec![PointId(11), PointId(12)], true);
    assert_eq!(s.points, vec![PointId(10), PointId(11), PointId(12)]);

    let m = mask_of(vec![square(1, 0.0, 0.0, 10.0, 10.0)]);
    s.set(NodeId(1), vec![PointId(10), PointId(99)], false);
    s.prune(&m);
    assert_eq!(s.points, vec![PointId(10)], "points that no longer exist are dropped");
    s.set(NodeId(7), vec![PointId(1)], false);
    s.prune(&m);
    assert_eq!(s, Selection::default(), "a deleted shape clears the selection");
}

#[test]
fn views_apply_the_whole_transform_chain() {
    let mut sh = square(1, 0.0, 0.0, 10.0, 10.0);
    sh.transform.translate = V2::new(100.0, 0.0);
    sh.points[1].tangent_out = V2::new(4.0, 0.0);
    sh.points[1].feather_pos = V2::new(0.0, -3.0);
    let mut g = Group::new(NodeId(5), "g");
    g.transform.translate = V2::new(0.0, 50.0);
    g.children.push(Node::Shape(sh));
    let mut m = RotoMask::default();
    m.root.transform.translate = V2::new(1.0, 2.0);
    m.root.children.push(Node::Group(g));
    let views = shape_views(&m);
    assert_eq!(views.len(), 1);
    let p = &views[0].points[1];
    // Local (10, 0): shape +100 x, group +50 y, root (+1, +2).
    assert!(near(p.pos, [111.0, 52.0]), "{:?}", p.pos);
    assert!(near(p.tangent_out, [115.0, 52.0]));
    assert!(near(p.feather.unwrap(), [111.0, 49.0]));
    assert!(views[0].points[0].feather.is_none());
    // Mapping back recovers local space.
    let local = to_local(&views[0].affine, p.pos).unwrap();
    assert!(near(local, [10.0, 0.0]), "{local:?}");
}

#[test]
fn rotated_shapes_map_drags_into_local_space() {
    let mut sh = square(1, 0.0, 0.0, 10.0, 10.0);
    sh.transform.rotate = 90.0;
    let m = mask_of(vec![sh]);
    let v = view_of_shape(&m, NodeId(1)).unwrap();
    // A 90 degree turn takes local (x, y) to document (-y, x).
    assert!(near(v.points[1].pos, [0.0, 10.0]), "{:?}", v.points[1].pos);
    let mut sel = Selection::default();
    sel.set(NodeId(1), vec![PointId(10)], false);
    let start = DragStart::capture(&m, &sel, NodeId(1), None).unwrap();
    // Dragging 10px to the right on screen moves the point 10px "up" in its local space.
    let params = start.move_params([10.0, 0.0], "k").unwrap();
    let pos = &params["moves"][0]["pos"];
    assert!((pos[0].as_f64().unwrap() - 0.0).abs() < 1e-9 && (pos[1].as_f64().unwrap() + 10.0).abs() < 1e-9, "{pos}");
    assert_eq!((params["shape"].as_u64(), params["coalesce"].as_str()), (Some(1), Some("k")));
}

#[test]
fn degenerate_transforms_produce_no_commands_instead_of_nan() {
    let mut sh = square(1, 0.0, 0.0, 10.0, 10.0);
    sh.transform.scale = V2::new(0.0, 1.0);
    let m = mask_of(vec![sh]);
    let mut sel = Selection::default();
    sel.set(NodeId(1), vec![PointId(10)], false);
    let start = DragStart::capture(&m, &sel, NodeId(1), None).unwrap();
    assert!(start.move_params([5.0, 5.0], "k").is_none());
    assert!(nudge_params(&m, &sel, [1.0, 0.0]).is_none());
    assert!(tangent_params(NodeId(1), PointId(10), true, [3.0, 3.0], &start.affine, [0.0, 0.0], "k").is_none());
    assert!(feather_params(NodeId(1), PointId(10), [3.0, 3.0], &start.affine, [0.0, 0.0], "k").is_none());
}

#[test]
fn hit_test_priorities_and_radius() {
    let mut a = square(1, 0.0, 0.0, 40.0, 40.0);
    a.points[1].tangent_out = V2::new(0.0, 12.0); // handle tip at (40, 12)
    a.points[1].feather_pos = V2::new(8.0, -8.0); // feather handle at (48, -8)
    let b = square(2, 100.0, 0.0, 140.0, 40.0);
    let m = mask_of(vec![a, b]);
    let views = shape_views(&m);
    let none = Selection::default();
    let mut sel = Selection::default();
    sel.set(NodeId(1), vec![PointId(11)], false);

    assert_eq!(hit_test(&views, &none, [1.0, 1.0], 6.0), Hit::Point { shape: NodeId(1), point: PointId(10) });
    assert!(matches!(hit_test(&views, &none, [120.0, 40.0], 3.0), Hit::Segment { shape: NodeId(2), index: 2, .. }), "on the bottom edge of the second square");
    assert_eq!(hit_test(&views, &none, [60.0, 20.0], 6.0), Hit::None, "empty canvas");
    assert_eq!(hit_test(&views, &none, [10.0, 7.0], 6.0), Hit::None, "outside the radius");

    // Handles only answer for selected points.
    assert!(
        matches!(hit_test(&views, &none, [40.0, 12.0], 4.0), Hit::Segment { shape: NodeId(1), index: 1, .. }),
        "unselected: a handle tip is just empty space beside the outline"
    );
    assert_eq!(hit_test(&views, &sel, [40.0, 12.0], 4.0), Hit::TangentOut { shape: NodeId(1), point: PointId(11) });
    assert_eq!(hit_test(&views, &sel, [48.0, -8.0], 4.0), Hit::Feather { shape: NodeId(1), point: PointId(11) });
    // The feather handle beats the anchor when both are in reach.
    assert_eq!(hit_test(&views, &sel, [44.0, -4.0], 10.0), Hit::Feather { shape: NodeId(1), point: PointId(11) });
}

#[test]
fn topmost_shape_wins_and_locked_or_hidden_shapes_are_skipped() {
    let mut top = square(2, 0.0, 0.0, 20.0, 20.0);
    let bottom = square(1, 0.0, 0.0, 20.0, 20.0);
    let m = mask_of(vec![bottom.clone(), top.clone()]);
    let sel = Selection::default();
    assert_eq!(hit_test(&shape_views(&m), &sel, [0.0, 0.0], 3.0), Hit::Point { shape: NodeId(2), point: PointId(20) });
    top.locked = true;
    let m = mask_of(vec![bottom.clone(), top.clone()]);
    assert_eq!(hit_test(&shape_views(&m), &sel, [0.0, 0.0], 3.0), Hit::Point { shape: NodeId(1), point: PointId(10) });
    top.locked = false;
    top.visible = false;
    let m = mask_of(vec![bottom, top]);
    assert_eq!(hit_test(&shape_views(&m), &sel, [0.0, 0.0], 3.0), Hit::Point { shape: NodeId(1), point: PointId(10) });
}

#[test]
fn segment_hits_report_the_segment_index() {
    let m = mask_of(vec![square(1, 0.0, 0.0, 40.0, 40.0)]);
    let views = shape_views(&m);
    let sel = Selection::default();
    match hit_test(&views, &sel, [20.0, 1.0], 3.0) {
        Hit::Segment { shape, index, pos } => {
            assert_eq!((shape, index), (NodeId(1), 0), "the top edge runs from point 0 to point 1");
            assert!((pos[0] - 20.0).abs() < 2.0 && pos[1].abs() < 1e-9);
        }
        other => panic!("{other:?}"),
    }
    // The closing edge (last point back to the first) is segment 3.
    assert!(matches!(hit_test(&views, &sel, [1.0, 20.0], 3.0), Hit::Segment { index: 3, .. }));
}

#[test]
fn open_shapes_have_no_closing_segment() {
    let mut s = square(1, 0.0, 0.0, 40.0, 40.0);
    s.closed = false;
    let views = shape_views(&mask_of(vec![s]));
    assert_eq!(hit_test(&views, &Selection::default(), [1.0, 20.0], 3.0), Hit::None);
    let samples = outline_samples(&views[0], 8);
    assert_eq!(samples.len(), 3 * 8 + 1);
    assert_eq!(samples.last().map(|s| s.1), Some([0.0, 40.0]));
}

#[test]
fn marquee_selects_points_and_picks_the_shape_with_most() {
    let m = mask_of(vec![square(1, 0.0, 0.0, 10.0, 10.0), square(2, 5.0, 5.0, 100.0, 100.0)]);
    let views = shape_views(&m);
    assert_eq!(points_in_rect(&views, NodeId(1), [-1.0, -1.0], [11.0, 11.0]).len(), 4);
    // Corner order does not matter.
    assert_eq!(points_in_rect(&views, NodeId(1), [11.0, 11.0], [-1.0, -1.0]).len(), 4);
    assert_eq!(points_in_rect(&views, NodeId(1), [-1.0, -1.0], [5.0, 5.0]), vec![PointId(10)]);
    assert!(points_in_rect(&views, NodeId(9), [-1e9, -1e9], [1e9, 1e9]).is_empty());
    assert_eq!(marquee_target(&views, [-1.0, -1.0], [11.0, 11.0]), Some(NodeId(1)));
    assert_eq!(marquee_target(&views, [50.0, 50.0], [101.0, 101.0]), Some(NodeId(2)));
    assert_eq!(marquee_target(&views, [200.0, 200.0], [300.0, 300.0]), None);
}

#[test]
fn handle_params_are_in_local_space() {
    let mut s = square(1, 10.0, 10.0, 50.0, 50.0);
    s.transform.translate = V2::new(100.0, 0.0);
    let m = mask_of(vec![s]);
    let v = view_of_shape(&m, NodeId(1)).unwrap();
    // Local point (10, 10) shows at (110, 10). Drag its out-handle tip to document (120, 10).
    let t = tangent_params(NodeId(1), PointId(10), true, [120.0, 10.0], &v.affine, [10.0, 10.0], "k").unwrap();
    assert_eq!(t["out"], json!([10.0, 0.0]));
    assert!(t.get("in").is_none());
    let t = tangent_params(NodeId(1), PointId(10), false, [105.0, 20.0], &v.affine, [10.0, 10.0], "k").unwrap();
    assert_eq!(t["in"], json!([-5.0, 10.0]));
    let f = feather_params(NodeId(1), PointId(10), [110.0, -5.0], &v.affine, [10.0, 10.0], "k").unwrap();
    assert_eq!(f["offset"], json!([0.0, -15.0]));
}

#[test]
fn nudge_moves_the_whole_selection() {
    let m = mask_of(vec![square(1, 0.0, 0.0, 10.0, 10.0)]);
    let mut sel = Selection::default();
    assert!(nudge_params(&m, &sel, [1.0, 0.0]).is_none(), "nothing selected");
    sel.set(NodeId(1), vec![PointId(10), PointId(12)], false);
    let p = nudge_params(&m, &sel, [1.0, -10.0]).unwrap();
    let moves = p["moves"].as_array().unwrap();
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[0]["pos"], json!([1.0, -10.0]));
    assert_eq!(moves[1]["pos"], json!([11.0, 0.0]));
    assert_eq!(p["coalesce"], "roto-nudge");
}

#[test]
fn feather_polyline_uses_handles_where_present() {
    let mut s = square(1, 0.0, 0.0, 10.0, 10.0);
    s.points[0].feather_pos = V2::new(-2.0, -2.0);
    let v = view_of_shape(&mask_of(vec![s]), NodeId(1)).unwrap();
    let poly = feather_polyline(&v);
    assert_eq!(poly.len(), 4);
    assert!(near(poly[0], [-2.0, -2.0]) && near(poly[1], [10.0, 0.0]));
}
