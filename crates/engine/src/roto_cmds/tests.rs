use photocraft_doc::roto::{Node, Shape};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use super::*;

fn session(w: u32, h: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "background": "white", "depth": 8})).unwrap();
    s
}

fn with_roto_layer(w: u32, h: u32) -> Session {
    let mut s = session(w, h);
    s.execute("layer.roto.add", json!({})).unwrap();
    s
}

fn mask(s: &Session) -> RotoMask {
    let d = s.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap().roto_mask.clone().unwrap()
}

fn revision(s: &Session) -> u64 {
    s.active().unwrap().revision
}

fn shape(s: &Session, id: u64) -> Shape {
    match mask(s).find(NodeId(id)) {
        Some(Node::Shape(sh)) => sh.clone(),
        _ => panic!("no shape {id}"),
    }
}

fn add_rect(s: &mut Session, x: f64, y: f64, w: f64, h: f64) -> u64 {
    s.execute("roto.node.add_rect", json!({"rect": [x, y, w, h]})).unwrap()["id"].as_u64().unwrap()
}

fn point_ids(sh: &Shape) -> Vec<u64> {
    sh.points.iter().map(|p| p.id.0).collect()
}

fn len(v: V2) -> f64 {
    v.x.hypot(v.y)
}

fn undo(s: &mut Session) {
    s.execute("edit.undo", json!({})).unwrap();
}

#[test]
fn add_info_delete_and_undo() {
    let mut s = session(64, 64);
    assert!(s.execute("roto.node.add_rect", json!({"rect": [1, 1, 5, 5]})).is_err(), "no roto mask yet");
    let info = s.execute("layer.roto.add", json!({})).unwrap();
    assert_eq!(info["enabled"], true);
    assert_eq!(info["density"], 1.0);
    assert_eq!(info["nodes"], json!([]));
    assert!(s.execute("layer.roto.add", json!({})).is_err(), "a second roto mask is refused");
    undo(&mut s);
    assert!(s.execute("layer.roto.info", json!({})).unwrap()["rotoMask"].is_null());
    s.execute("layer.roto.add", json!({})).unwrap();
    s.execute("layer.roto.delete", json!({})).unwrap();
    assert!(s.execute("layer.roto.info", json!({})).unwrap()["rotoMask"].is_null());
    undo(&mut s);
    assert!(s.execute("layer.roto.info", json!({})).unwrap()["enabled"].as_bool().unwrap());
}

#[test]
fn shape_tools_create_the_expected_geometry() {
    let mut s = with_roto_layer(200, 200);
    let rect = add_rect(&mut s, 10.0, 20.0, 30.0, 40.0);
    let r = shape(&s, rect);
    assert_eq!(r.points.len(), 4);
    assert_eq!((r.points[0].pos, r.points[2].pos), (V2::new(10.0, 20.0), V2::new(40.0, 60.0)));
    assert!(r.closed);

    let e = s.execute("roto.node.add_ellipse", json!({"rect": [50, 50, 100, 60], "name": "Eye"})).unwrap()["id"].as_u64().unwrap();
    let e = shape(&s, e);
    assert_eq!((e.name.as_str(), e.points.len()), ("Eye", 4));
    assert!(e.points.iter().all(|p| p.smooth));
    // The ellipse's area is close to pi * a * b.
    let m = mask(&s);
    let one = {
        let mut only = RotoMask::default();
        only.root.children.push(Node::Shape(Shape { id: NodeId(1), ..shape_clone(&m, e.id) }));
        only
    };
    let area: f64 = vector::roto::roto_values(&one, Rect::new(0, 0, 200, 200)).iter().map(|v| f64::from(*v)).sum();
    let want = std::f64::consts::PI * 50.0 * 30.0;
    assert!((area - want).abs() / want < 0.01, "{area} vs {want}");

    // Freehand: a noisy circle of 240 samples is fitted to far fewer bezier points.
    let pts: Vec<Value> = (0..240)
        .map(|i| {
            let a = f64::from(i) / 240.0 * std::f64::consts::TAU;
            let r = 40.0 + (f64::from(i) * 7.3).sin() * 0.4;
            json!([100.0 + a.cos() * r, 100.0 + a.sin() * r])
        })
        .collect();
    let id = s.execute("roto.node.add_freehand", json!({"points": pts, "tolerance": 1.5})).unwrap()["id"].as_u64().unwrap();
    let f = shape(&s, id);
    assert!(f.points.len() >= 3 && f.points.len() < 60, "{} points", f.points.len());
    assert!(f.closed);
    assert!(s.execute("roto.node.add_freehand", json!({"points": [[1, 1]]})).is_err(), "needs two points");
}

fn shape_clone(m: &RotoMask, id: NodeId) -> Shape {
    match m.find(id) {
        Some(Node::Shape(sh)) => sh.clone(),
        _ => panic!("shape"),
    }
}

#[test]
fn feather_commands_and_undo() {
    let mut s = with_roto_layer(100, 100);
    let id = add_rect(&mut s, 20.0, 20.0, 40.0, 40.0);
    let ids = point_ids(&shape(&s, id));

    // Pull every point's soft edge out by 4px at once: each offset is 4px long, pointing outward.
    s.execute("roto.feather.set_all", json!({"distance": 4.0})).unwrap();
    let sh = shape(&s, id);
    assert!(sh.points.iter().all(|p| (len(p.feather_pos) - 4.0).abs() < 1e-9));
    assert!(sh.points[0].feather_pos.x < 0.0 && sh.points[0].feather_pos.y < 0.0, "top-left corner feathers up-left");
    assert!(sh.points[2].feather_pos.x > 0.0 && sh.points[2].feather_pos.y > 0.0, "bottom-right corner feathers down-right");

    // Scale only two points: they double, the others stay.
    s.execute("roto.feather.scale_selected", json!({"shape": id, "ids": [ids[0], ids[1]], "factor": 2.0})).unwrap();
    let sh = shape(&s, id);
    assert!((len(sh.points[0].feather_pos) - 8.0).abs() < 1e-9 && (len(sh.points[1].feather_pos) - 8.0).abs() < 1e-9);
    assert!((len(sh.points[2].feather_pos) - 4.0).abs() < 1e-9);

    // One point by hand; inward feathering with a negative distance; zero clears.
    s.execute("roto.feather.set_point", json!({"shape": id, "id": ids[3], "offset": [-3.0, 0.5]})).unwrap();
    assert_eq!(shape(&s, id).points[3].feather_pos, V2::new(-3.0, 0.5));
    s.execute("roto.feather.set_all", json!({"distance": -5.0})).unwrap();
    assert!(shape(&s, id).points[0].feather_pos.x > 0.0, "negative distance feathers inward");
    s.execute("roto.feather.set_all", json!({"distance": 0.0})).unwrap();
    assert!(shape(&s, id).points.iter().all(|p| p.feather_pos == V2::ZERO && p.feather_out == V2::ZERO));

    // Each command was one undo step, back to the 4px feather.
    undo(&mut s);
    undo(&mut s);
    undo(&mut s);
    assert!((len(shape(&s, id).points[0].feather_pos) - 8.0).abs() < 1e-9);
    undo(&mut s);
    assert!((len(shape(&s, id).points[0].feather_pos) - 4.0).abs() < 1e-9);
}

#[test]
fn set_all_on_an_ellipse_gives_an_even_soft_edge() {
    let mut s = with_roto_layer(120, 120);
    s.execute("roto.node.add_ellipse", json!({"rect": [20, 20, 60, 60]})).unwrap();
    s.execute("roto.feather.set_all", json!({"distance": 10.0})).unwrap();
    let v = vector::roto::roto_values(&mask(&s), Rect::new(0, 0, 120, 120));
    // Radius 30 around (50, 50): 5px outside the circle is halfway through the band at any angle.
    for deg in [0.0f64, 30.0, 45.0, 60.0, 135.0, 250.0] {
        let (sn, cs) = deg.to_radians().sin_cos();
        let (x, y) = (50.0 + cs * 35.0, 50.0 + sn * 35.0);
        let a = v[y.floor() as usize * 120 + x.floor() as usize];
        assert!((a - 0.5).abs() < 0.12, "angle {deg}: {a}");
    }
}

#[test]
fn bad_input_is_an_error_and_never_changes_the_document() {
    let mut s = with_roto_layer(100, 100);
    let id = add_rect(&mut s, 10.0, 10.0, 20.0, 20.0);
    let before = mask(&s);
    let rev = revision(&s);
    let cases: Vec<(&str, Value)> = vec![
        ("roto.point.move", json!({"shape": id, "moves": [{"id": 999999, "pos": [1, 1]}]})),
        ("roto.point.move", json!({"shape": id, "moves": [{"id": 1, "pos": [1e300, 1]}]})),
        ("roto.point.move", json!({"shape": id, "moves": [{"id": 1, "pos": "x"}]})),
        ("roto.point.delete", json!({"shape": id, "ids": [424242]})),
        ("roto.point.set", json!({"shape": 777, "id": 1})),
        ("roto.node.set", json!({"id": id, "opacity": 2.0})),
        ("roto.node.set", json!({"id": id, "opacity": -0.1})),
        ("roto.node.set", json!({"id": id, "blendOp": "nope"})),
        ("roto.node.set", json!({"id": id, "blur": 1e9})),
        ("roto.node.set", json!({"id": 31337, "visible": false})),
        ("roto.node.delete", json!({"ids": [31337]})),
        ("roto.node.reorder", json!({"id": 31337, "index": 0})),
        ("roto.node.add_shape", json!({"points": [[0, 0], [1e300, 5]]})),
        ("roto.node.add_shape", json!({"points": "nope"})),
        ("roto.node.add_rect", json!({"rect": [0, 0, -5, 5]})),
        ("roto.node.transform", json!({"id": id, "scale": [1e9, 1]})),
        ("roto.node.transform", json!({"id": id, "skew": 90.0})),
        ("roto.feather.set_all", json!({"distance": 1e9})),
        ("roto.feather.scale_selected", json!({"shape": id, "factor": 1e9})),
        ("roto.instance.set", json!({"density": 1.5})),
        ("roto.instance.set", json!({"overlap": "wat"})),
        ("roto.instance.set", json!({"backend": "tpu"})),
    ];
    for (cmd, params) in cases {
        assert!(s.execute(cmd, params.clone()).is_err(), "{cmd} {params} should be an error");
    }
    assert_eq!(mask(&s), before, "failed commands leave the mask untouched");
    assert_eq!(revision(&s), rev, "and record no history");
}

#[test]
fn group_ungroup_reorder_duplicate_delete() {
    let mut s = with_roto_layer(100, 100);
    let a = add_rect(&mut s, 0.0, 0.0, 10.0, 10.0);
    let b = add_rect(&mut s, 20.0, 0.0, 10.0, 10.0);
    let c = add_rect(&mut s, 40.0, 0.0, 10.0, 10.0);
    let g = s.execute("roto.node.group", json!({"ids": [a, c], "name": "Pair"})).unwrap()["id"].as_u64().unwrap();
    let top: Vec<u64> = mask(&s)
        .root
        .children
        .iter()
        .map(|n| match n {
            Node::Group(g) => g.id.0,
            Node::Shape(sh) => sh.id.0,
        })
        .collect();
    assert_eq!(top, vec![g, b]);
    assert!(s.execute("roto.node.group", json!({"ids": [a, b]})).is_err(), "a and b no longer share a parent");

    // A new shape can be created inside the group.
    let inner = s.execute("roto.node.add_rect", json!({"rect": [60, 0, 10, 10], "parent": g})).unwrap()["id"].as_u64().unwrap();
    assert_eq!(mask(&s).locate(NodeId(inner)).map(|l| l.0), Some(NodeId(g)));

    s.execute("roto.node.reorder", json!({"id": b, "index": 0})).unwrap();
    assert!(matches!(mask(&s).root.children[0], Node::Shape(_)));

    // Ungroup is refused while the group is not neutral, and works once it is.
    s.execute("roto.node.set", json!({"id": g, "opacity": 0.5})).unwrap();
    assert!(s.execute("roto.node.ungroup", json!({"id": g})).is_err());
    s.execute("roto.node.set", json!({"id": g, "opacity": 1.0})).unwrap();
    let ids = s.execute("roto.node.ungroup", json!({"id": g})).unwrap();
    assert_eq!(ids["ids"], json!([a, c, inner]));

    let copies = s.execute("roto.node.duplicate", json!({"ids": [a]})).unwrap()["ids"].as_array().unwrap().clone();
    assert_eq!(copies.len(), 1);
    assert_ne!(copies[0].as_u64().unwrap(), a);
    assert_eq!(shape(&s, copies[0].as_u64().unwrap()).points.len(), 4);
    s.execute("roto.node.rename", json!({"id": a, "name": "Left cheek"})).unwrap();
    assert_eq!(shape(&s, a).name, "Left cheek");
    assert!(s.execute("roto.node.rename", json!({"id": a, "name": ""})).is_err());

    // Delete a parent and one of its children in the same call: not an error.
    let g2 = s.execute("roto.node.group", json!({"ids": [b, c]})).unwrap()["id"].as_u64().unwrap();
    s.execute("roto.node.delete", json!({"ids": [g2, b]})).unwrap();
    assert!(mask(&s).find(NodeId(b)).is_none() && mask(&s).find(NodeId(c)).is_none());
    undo(&mut s);
    assert!(mask(&s).find(NodeId(b)).is_some());
}

#[test]
fn points_add_move_set_delete_and_smooth_mirroring() {
    let mut s = with_roto_layer(100, 100);
    let id = add_rect(&mut s, 10.0, 10.0, 30.0, 30.0);
    let p0 = point_ids(&shape(&s, id))[0];
    let added = s.execute("roto.point.add", json!({"shape": id, "pos": [25, 5], "index": 1})).unwrap();
    assert_eq!(added["index"], 1);
    let new_id = added["id"].as_u64().unwrap();
    assert_eq!(shape(&s, id).points[1].id.0, new_id);
    assert!(!point_ids(&shape(&s, id))[..1].contains(&new_id) && new_id > p0);

    s.execute("roto.point.move", json!({"shape": id, "moves": [{"id": new_id, "pos": [26, 4]}]})).unwrap();
    assert_eq!(shape(&s, id).points[1].pos, V2::new(26.0, 4.0));

    // A smooth point keeps its handles collinear: setting `in` mirrors `out`.
    s.execute("roto.point.set", json!({"shape": id, "id": new_id, "smooth": true, "in": [-3, 1]})).unwrap();
    let q = shape(&s, id).points[1];
    assert!(q.smooth && q.tangent_in == V2::new(-3.0, 1.0) && q.tangent_out == V2::new(3.0, -1.0), "{q:?}");
    // Both given: no mirroring.
    s.execute("roto.point.set", json!({"shape": id, "id": new_id, "in": [-2, 0], "out": [5, 5]})).unwrap();
    let q = shape(&s, id).points[1];
    assert_eq!((q.tangent_in, q.tangent_out), (V2::new(-2.0, 0.0), V2::new(5.0, 5.0)));

    s.execute("roto.point.delete", json!({"shape": id, "ids": [new_id]})).unwrap();
    assert_eq!(shape(&s, id).points.len(), 4);
}

#[test]
fn node_and_point_transforms() {
    let mut s = with_roto_layer(100, 100);
    let id = add_rect(&mut s, 10.0, 10.0, 10.0, 10.0);
    let ids = point_ids(&shape(&s, id));
    // A non-destructive node transform: the points do not change, the rendering does.
    s.execute("roto.node.transform", json!({"id": id, "translate": [30, 0]})).unwrap();
    assert_eq!(shape(&s, id).points[0].pos, V2::new(10.0, 10.0));
    let v = vector::roto::roto_values(&mask(&s), Rect::new(0, 0, 100, 100));
    assert!((v[15 * 100 + 45] - 1.0).abs() < 1e-5 && v[15 * 100 + 15].abs() < 1e-5);
    s.execute("roto.node.transform", json!({"id": id, "translate": [0, 0]})).unwrap();

    // A baked point transform: rotate the top-right and bottom-right points 90 degrees about (10, 10).
    s.execute("roto.point.transform", json!({"shape": id, "ids": [ids[1], ids[2]], "rotate": 90.0, "pivot": [10, 10]})).unwrap();
    let sh = shape(&s, id);
    let p = sh.points[1].pos; // (20, 10) -> (10, 20)
    assert!((p.x - 10.0).abs() < 1e-9 && (p.y - 20.0).abs() < 1e-9, "{p:?}");
    assert_eq!(sh.points[0].pos, V2::new(10.0, 10.0), "unselected points stay");
    assert!(s.execute("roto.point.transform", json!({"shape": id, "ids": [31337], "rotate": 5.0})).is_err());
}

#[test]
fn per_shape_settings_reach_the_rendered_mask() {
    let mut s = with_roto_layer(60, 60);
    let a = add_rect(&mut s, 0.0, 0.0, 60.0, 60.0);
    let b = add_rect(&mut s, 20.0, 20.0, 20.0, 20.0);
    s.execute("roto.node.set", json!({"id": b, "blendOp": "subtract"})).unwrap();
    s.execute("roto.node.set", json!({"id": a, "opacity": 0.5})).unwrap();
    s.execute("roto.instance.set", json!({"density": 0.8})).unwrap();
    let v = vector::roto::roto_values(&mask(&s), Rect::new(0, 0, 60, 60));
    assert!((v[5 * 60 + 5] - 0.4).abs() < 1e-5, "0.5 shape opacity x 0.8 density: {}", v[5 * 60 + 5]);
    assert!(v[30 * 60 + 30].abs() < 1e-5, "the subtracted square is a hole");
    let info = s.execute("layer.roto.info", json!({})).unwrap();
    assert_eq!(info["nodes"][1]["blendOp"], "subtract");
    assert_eq!(info["density"].as_f64().map(|d| (d * 10.0).round()), Some(8.0));
}

#[test]
fn rasterize_bakes_the_roto_mask_into_the_pixel_mask() {
    let mut s = with_roto_layer(40, 30);
    add_rect(&mut s, 5.0, 5.0, 20.0, 10.0);
    s.execute("roto.feather.set_all", json!({"distance": 3.0})).unwrap();
    let want = vector::roto::roto_values(&mask(&s), Rect::new(0, 0, 40, 30));
    s.execute("layer.rasterize.rotoMask", json!({})).unwrap();
    let d = s.active().unwrap();
    let l = d.doc.layer(d.active_layer.unwrap()).unwrap();
    assert!(l.roto_mask.is_none());
    let got = l.mask.as_ref().unwrap().surface.read_region(Rect::new(0, 0, 40, 30));
    // The 8-bit pixel mask quantizes to 1/255.
    assert!(got.iter().zip(&want).all(|(a, b)| (a - b).abs() < 0.003));
    undo(&mut s);
    assert!(mask(&s).find(NodeId(1)).is_some());
}

#[test]
fn a_linked_roto_mask_follows_its_layer() {
    // The Background layer is position-locked: use a regular layer.
    let mut s = session(100, 100);
    s.execute("layer.new.layer", json!({"name": "Movable"})).unwrap();
    s.execute("layer.roto.add", json!({})).unwrap();
    let id = add_rect(&mut s, 10.0, 10.0, 20.0, 20.0);
    s.execute("layer.translate", json!({"dx": 7, "dy": 3})).unwrap();
    let render = |s: &Session| vector::roto::roto_values(&mask(s), Rect::new(0, 0, 100, 100));
    let v = render(&s);
    assert!((v[15 * 100 + 20] - 1.0).abs() < 1e-5, "inside the moved square");
    assert!(v[12 * 100 + 12].abs() < 1e-5, "the old top-left corner is empty now");
    assert_eq!(shape(&s, id).points[0].pos, V2::new(10.0, 10.0), "the move rides on the root transform");
    // Unlinked: the mask stays put.
    s.execute("roto.instance.set", json!({"linked": false})).unwrap();
    s.execute("layer.translate", json!({"dx": 50, "dy": 0})).unwrap();
    assert!((render(&s)[15 * 100 + 20] - 1.0).abs() < 1e-5);
}
