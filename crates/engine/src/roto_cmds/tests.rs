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

const NUKE_SAMPLE: &str = include_str!("../../../io/tests/fixtures/nuke-roto-bezier-1.nk");

#[test]
fn nuke_export_and_import_round_trip_through_commands() {
    let mut s = with_roto_layer(2048, 1556);
    let imported = s.execute("roto.import_nuke", json!({"text": NUKE_SAMPLE})).unwrap();
    assert_eq!(imported["shapes"], 1);
    let sh = shape(&s, 1);
    assert_eq!((sh.name.as_str(), sh.points.len()), ("Bezier1", 6));
    assert!((sh.points[0].pos.y - 402.0).abs() < 1e-3, "y is flipped by the document height: {}", sh.points[0].pos.y);

    let out = s.execute("roto.export_nuke", json!({})).unwrap();
    assert_eq!(out["shapes"], 1);
    let text = out["text"].as_str().unwrap().to_string();
    assert!(text.starts_with("set cut_paste_input [stack 0]") && text.contains("Roto {"));

    // Replace mode swaps the content; append mode adds to it with fresh ids.
    s.execute("roto.import_nuke", json!({"text": text, "mode": "replace"})).unwrap();
    assert_eq!(mask(&s).root.children.len(), 1);
    s.execute("roto.import_nuke", json!({"text": text, "mode": "append"})).unwrap();
    let m = mask(&s);
    assert_eq!(m.root.children.len(), 2);
    let (a, b) = match (&m.root.children[0], &m.root.children[1]) {
        (Node::Shape(a), Node::Shape(b)) => (a, b),
        _ => panic!("two shapes"),
    };
    assert_ne!(a.id, b.id);
    assert!(a.points.iter().all(|p| b.points.iter().all(|q| q.id != p.id)), "point ids are unique across shapes");
    undo(&mut s);
    assert_eq!(mask(&s).root.children.len(), 1, "an import is one undo step");
}

#[test]
fn nuke_import_creates_the_mask_reports_warnings_and_rejects_bad_text() {
    let mut s = session(2048, 1556);
    let noisy = NUKE_SAMPLE.replace("{a osw x41200000", "{a zzz 3 osw x41200000");
    let r = s.execute("roto.import_nuke", json!({"text": noisy})).unwrap();
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.contains("zzz"))), "{r}");
    assert_eq!(mask(&s).root.children.len(), 1, "a missing roto mask is created by the import");

    let before = mask(&s);
    let rev = revision(&s);
    for bad_params in
        [json!({"text": "Blur { size 3 }"}), json!({"text": "Roto {"}), json!({}), json!({"text": 5}), json!({"text": NUKE_SAMPLE, "mode": "merge"})]
    {
        assert!(s.execute("roto.import_nuke", bad_params.clone()).is_err(), "{bad_params}");
    }
    assert_eq!(mask(&s), before);
    assert_eq!(revision(&s), rev);
}

// ---------------------------------------------------------------------------------------------
// Other engine features that strip, apply or bake masks must treat a roto mask like the others.

fn pixel_layer(s: &mut Session, name: &str) -> photocraft_doc::LayerId {
    s.execute("layer.new.layer", json!({"name": name})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap();
    let st = s.active_mut().unwrap();
    let doc = std::sync::Arc::make_mut(&mut st.doc);
    let (w, h) = (doc.size.width as i32, doc.size.height as i32);
    doc.layer_mut(id).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, w, h), &[1.0, 0.0, 0.0, 1.0]);
    id
}

#[test]
fn flatten_all_masks_folds_a_roto_mask_into_the_pixels_exactly_once() {
    let mut s = session(80, 60);
    let id = pixel_layer(&mut s, "Px");
    s.execute("layer.roto.add", json!({})).unwrap();
    add_rect(&mut s, 20.0, 15.0, 40.0, 30.0);
    s.execute("roto.feather.set_all", json!({"distance": 8.0})).unwrap();
    let before = photocraft_compose::flatten(&s.active().unwrap().doc);
    s.execute("file.scripts.flattenAllMasks", json!({})).unwrap();
    let doc = &s.active().unwrap().doc;
    let l = doc.layer(id).unwrap();
    assert!(l.roto_mask.is_none() && l.mask.is_none(), "the mask now lives in the pixels");
    let after = photocraft_compose::flatten(doc);
    let worst = before.px.iter().zip(&after.px).map(|(a, b)| (0..4).map(|k| (a[k] - b[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
    assert!(worst <= 1.0 / 255.0 + 1e-5, "the picture is unchanged (a double-applied feather would square it): {worst}");
    // And the soft edge really is in there: over the white background a band pixel is a pink
    // mix, neither solid red nor white.
    let px = after.px[(22 * 80 + 18) as usize];
    assert!(px[1] > 0.05 && px[1] < 0.95, "{px:?}");
}

#[test]
fn flatten_all_layer_effects_does_not_apply_a_roto_mask_twice() {
    let mut s = session(80, 60);
    let id = pixel_layer(&mut s, "Fx");
    s.execute("layer.roto.add", json!({})).unwrap();
    add_rect(&mut s, 20.0, 15.0, 40.0, 30.0);
    s.execute("roto.feather.set_all", json!({"distance": 8.0})).unwrap();
    {
        let st = s.active_mut().unwrap();
        let doc = std::sync::Arc::make_mut(&mut st.doc);
        doc.layer_mut(id).unwrap().effects.items.push(photocraft_doc::Effect::default_drop_shadow());
    }
    let before = photocraft_compose::flatten(&s.active().unwrap().doc);
    s.execute("file.scripts.flattenAllLayerEffects", json!({})).unwrap();
    let doc = &s.active().unwrap().doc;
    assert!(doc.layer(id).unwrap().roto_mask.is_none(), "the mask was baked with the effects");
    let after = photocraft_compose::flatten(doc);
    let worst = before.px.iter().zip(&after.px).map(|(a, b)| (0..4).map(|k| (a[k] - b[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
    assert!(worst <= 2.0 / 255.0, "the picture is unchanged: {worst}");
}

#[test]
fn content_alone_ignores_the_roto_mask() {
    let mut s = session(40, 30);
    s.execute("layer.newFillLayer.solidColor", json!({})).unwrap();
    s.execute("layer.roto.add", json!({})).unwrap();
    add_rect(&mut s, 30.0, 20.0, 5.0, 5.0);
    let st = s.active().unwrap();
    let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
    let px = crate::extra_cmds::content_pixels(&st.doc, l).read_region(Rect::new(2, 2, 3, 3));
    assert!(px.last().is_some_and(|a| *a > 0.99), "far from the roto shape the content is still opaque: {px:?}");
}

#[test]
fn a_lut_export_ignores_roto_masks_on_adjustment_layers() {
    let mut s = session(40, 30);
    {
        let st = s.active_mut().unwrap();
        let doc = std::sync::Arc::make_mut(&mut st.doc);
        let mut adj = photocraft_doc::Layer::new("inv", photocraft_doc::LayerContent::Adjustment(photocraft_doc::Adjustment::Invert));
        // A mask that hides everything the lattice covers: masks are spatial, a LUT is not.
        let mut m = RotoMask::default();
        let mut sh = Shape::new(NodeId(1), "far");
        for (i, (x, y)) in [(5000.0, 5000.0), (5010.0, 5000.0), (5010.0, 5010.0)].into_iter().enumerate() {
            sh.points.push(photocraft_doc::roto::Point::corner(PointId(i as u64 + 1), x, y));
        }
        m.root.children.push(Node::Shape(sh));
        adj.roto_mask = Some(m);
        doc.layers.push(adj);
    }
    let cube = crate::file_cmds::bake_cube(&s.active().unwrap().doc, 2, "t");
    let first = cube.lines().find(|l| l.split_whitespace().count() == 3 && l.split_whitespace().all(|t| t.parse::<f32>().is_ok())).unwrap_or("");
    let v: Vec<f32> = first.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    assert!(v.iter().all(|x| *x > 0.99), "the black corner of the cube is inverted: {first}");
}

#[test]
fn shifting_a_layer_moves_its_roto_mask() {
    let mut l = photocraft_doc::Layer::raster("l", photocraft_color::PixelFormat::RGBA8);
    let mut m = RotoMask::default();
    let mut sh = Shape::new(NodeId(1), "s");
    for (i, (x, y)) in [(10.0, 10.0), (30.0, 10.0), (30.0, 30.0), (10.0, 30.0)].into_iter().enumerate() {
        sh.points.push(photocraft_doc::roto::Point::corner(PointId(i as u64 + 1), x, y));
    }
    m.root.children.push(Node::Shape(sh));
    l.roto_mask = Some(m);
    crate::smart_cmds::shift_layer(&mut l, 7, 3);
    let v = vector::roto::roto_values(l.roto_mask.as_ref().unwrap(), Rect::new(0, 0, 60, 60));
    assert!((v[20 * 60 + 25] - 1.0).abs() < 1e-5, "inside the moved square");
    assert!(v[12 * 60 + 12].abs() < 1e-5, "the old corner is empty now");
}

#[test]
fn a_maximum_size_freehand_stroke_is_fitted_quickly_and_one_past_it_is_refused() {
    let mut s = with_roto_layer(1000, 1000);
    let n = 100_000;
    // A wobbling spiral: no two samples alike, so nothing collapses before the fit.
    let pts: Vec<Value> = (0..n)
        .map(|i| {
            let t = f64::from(i) / f64::from(n);
            let r = 100.0 + 300.0 * t + (t * 900.0).sin() * 3.0;
            json!([500.0 + r * (t * 40.0).cos(), 500.0 + r * (t * 40.0).sin()])
        })
        .collect();
    let started = std::time::Instant::now();
    let r = s.execute("roto.node.add_freehand", json!({"points": pts, "tolerance": 1.0, "closed": false})).unwrap();
    let took = started.elapsed();
    let count = r["points"].as_array().unwrap().len();
    assert!((10..5000).contains(&count), "{count} fitted points");
    assert!(took.as_secs() < 20, "fitting {n} samples took {took:?}");
    let too_many: Vec<Value> = (0..=n).map(|i| json!([f64::from(i), 0.0])).collect();
    assert!(s.execute("roto.node.add_freehand", json!({"points": too_many})).is_err());
}

#[test]
fn view_roto_edit_is_view_state_with_no_history_and_keeps_a_clean_document_clean() {
    use vector::roto::{editing_layer, set_editing_layer};
    set_editing_layer(None);
    let mut s = with_roto_layer(40, 30);
    let id = s.active().unwrap().active_layer.unwrap();
    // Mark the document clean, as just after saving.
    let st = s.active_mut().unwrap();
    st.saved_revision = st.revision;
    let (rev, clean) = (st.revision, true);

    let r = s.execute("view.rotoEdit", json!({"on": true})).unwrap();
    assert_eq!((r["editing"].as_bool(), editing_layer()), (Some(true), Some(id)));
    let st = s.active().unwrap();
    assert!(st.revision > rev, "the canvas must recomposite");
    assert!(st.last_damage.is_none(), "everything may have changed on screen");
    assert_eq!(st.saved_revision == st.revision, clean, "a view change never dirties the document");
    // Not an undo step: undo takes back adding the mask, not the view.
    undo(&mut s);
    assert!(s.active().unwrap().doc.layer(id).unwrap().roto_mask.is_none(), "undo took back adding the roto mask, not the view");

    // Toggling, and turning off for a layer that is not the edited one, leaves the edit alone.
    set_editing_layer(None);
    let mut s = with_roto_layer(40, 30);
    let id = s.active().unwrap().active_layer.unwrap();
    s.execute("view.rotoEdit", json!({})).unwrap();
    assert_eq!(editing_layer(), Some(id), "no `on` toggles it on");
    s.execute("view.rotoEdit", json!({"layer": id.0 + 1000, "on": false})).ok();
    assert_eq!(editing_layer(), Some(id), "another layer's off does not end this edit");
    s.execute("view.rotoEdit", json!({})).unwrap();
    assert_eq!(editing_layer(), None, "toggled off");

    // A layer without a roto mask cannot be edited.
    set_editing_layer(None);
    let mut plain = session(40, 30);
    assert!(plain.execute("view.rotoEdit", json!({"on": true})).is_err());
    assert_eq!(editing_layer(), None);
    assert!(plain.execute("view.rotoEdit", json!({"on": "yes"})).is_err());
}

#[test]
fn cusp_uncusp_and_smooth_commands_edit_the_points_one_undo_step_each() {
    let mut s = with_roto_layer(100, 100);
    let id = add_rect(&mut s, 20.0, 20.0, 40.0, 40.0);
    let ids = point_ids(&shape(&s, id));
    let area = |s: &Session| -> f64 { vector::roto::roto_values(&mask(s), Rect::new(0, 0, 100, 100)).iter().map(|v| f64::from(*v)).sum() };
    let square = area(&s);

    // Smooth: corners become curves. A smooth curve through four corners bows outward past the
    // square, so the shape is no longer the square (here about a third bigger).
    let r = s.execute("roto.point.smooth", json!({"shape": id})).unwrap();
    assert_eq!(r["changed"], 4);
    let sh = shape(&s, id);
    assert!(sh.points.iter().all(|p| p.smooth && len(p.tangent_out) > 1.0 && (len(p.tangent_in) - len(p.tangent_out)).abs() < 1e-9));
    let rounded = area(&s);
    assert!((rounded - square).abs() > 20.0 && rounded < square * 1.6, "{rounded} vs {square}");

    // Cusp one point: its handles stay, the link is gone; the others are untouched.
    let handles = sh.points[0].tangent_out;
    s.execute("roto.point.cusp", json!({"shape": id, "ids": [ids[0]]})).unwrap();
    let sh = shape(&s, id);
    assert!(!sh.points[0].smooth && sh.points[1].smooth);
    assert_eq!(sh.points[0].tangent_out, handles);
    // The handles are now independent: bend one, the other stays.
    s.execute("roto.point.set", json!({"shape": id, "id": ids[0], "out": [5, 25]})).unwrap();
    let sh = shape(&s, id);
    assert_eq!(sh.points[0].tangent_out, V2::new(5.0, 25.0));
    assert!(len(sh.points[0].tangent_in) > 1.0 && sh.points[0].tangent_in != V2::new(-5.0, -25.0));

    // Uncusp: linked again, in-handle opposite the out-handle, keeping its length.
    let in_len = len(sh.points[0].tangent_in);
    s.execute("roto.point.uncusp", json!({"shape": id, "ids": [ids[0]]})).unwrap();
    let p = shape(&s, id).points[0];
    assert!(p.smooth);
    assert!((len(p.tangent_in) - in_len).abs() < 1e-9);
    let cross = p.tangent_in.x * p.tangent_out.y - p.tangent_in.y * p.tangent_out.x;
    assert!(cross.abs() < 1e-6 && p.tangent_in.x * p.tangent_out.x + p.tangent_in.y * p.tangent_out.y < 0.0, "opposite and collinear");

    // Each command was one undo step.
    undo(&mut s);
    assert!(!shape(&s, id).points[0].smooth, "undo of uncusp: still cusped");
    undo(&mut s);
    undo(&mut s);
    assert!(shape(&s, id).points[0].smooth, "undo of cusp (after undoing the handle edit)");
    undo(&mut s);
    assert!(shape(&s, id).points.iter().all(|p| p.tangent_out == V2::ZERO && !p.smooth), "undo of smooth: the square again");
}

#[test]
fn point_shape_commands_reject_bad_ids_without_touching_anything() {
    let mut s = with_roto_layer(100, 100);
    let id = add_rect(&mut s, 20.0, 20.0, 40.0, 40.0);
    let before = mask(&s);
    let rev = revision(&s);
    for cmd in ["roto.point.cusp", "roto.point.uncusp", "roto.point.smooth"] {
        assert!(s.execute(cmd, json!({"shape": id, "ids": [424242]})).is_err(), "{cmd}: unknown point");
        assert!(s.execute(cmd, json!({"shape": 777})).is_err(), "{cmd}: unknown shape");
        assert!(s.execute(cmd, json!({"shape": id, "ids": "all"})).is_err(), "{cmd}: ids must be a list");
        assert!(s.execute(cmd, json!({})).is_err(), "{cmd}: a shape is required");
    }
    assert_eq!(mask(&s), before);
    assert_eq!(revision(&s), rev);
}

fn selection_values(s: &Session) -> Vec<f32> {
    let d = s.active().unwrap();
    photocraft_algo::selection::mask_from_surface(d.doc.selection.as_ref(), d.doc.bounds())
}

fn selected_area(s: &Session) -> f64 {
    selection_values(s).iter().map(|v| f64::from(*v)).sum()
}

#[test]
fn selection_from_a_shape_a_group_or_the_whole_mask() {
    let mut s = with_roto_layer(100, 100);
    let a = add_rect(&mut s, 0.0, 0.0, 20.0, 20.0);
    let b = add_rect(&mut s, 30.0, 0.0, 20.0, 20.0);
    let c = add_rect(&mut s, 60.0, 0.0, 20.0, 20.0);
    let g = s.execute("roto.node.group", json!({"ids": [b, c]})).unwrap()["id"].as_u64().unwrap();
    let one = |s: &mut Session, params: Value| {
        let r = s.execute("roto.selection.make", params).unwrap();
        (selected_area(s), r["shapes"].as_u64().unwrap())
    };
    assert_eq!(one(&mut s, json!({"node": a})), (400.0, 1));
    assert_eq!(one(&mut s, json!({"node": g})), (800.0, 2));
    assert_eq!(one(&mut s, json!({})), (1200.0, 3));
    assert_eq!(one(&mut s, json!({"node": 0})), (1200.0, 3));
    // Modes combine with the existing selection.
    s.execute("roto.selection.make", json!({"node": a})).unwrap();
    s.execute("roto.selection.make", json!({"node": g, "mode": "add"})).unwrap();
    assert_eq!(selected_area(&s), 1200.0);
    s.execute("roto.selection.make", json!({"node": b, "mode": "subtract"})).unwrap();
    assert_eq!(selected_area(&s), 800.0);
    // One undo step.
    undo(&mut s);
    assert_eq!(selected_area(&s), 1200.0);
}

#[test]
fn selection_each_spline_ignores_the_blend_ops_between_them() {
    let mut s = with_roto_layer(100, 100);
    let big = add_rect(&mut s, 0.0, 0.0, 40.0, 40.0);
    let hole = add_rect(&mut s, 10.0, 10.0, 20.0, 20.0);
    s.execute("roto.node.set", json!({"id": hole, "blendOp": "subtract"})).unwrap();
    let _ = big;
    s.execute("roto.selection.make", json!({})).unwrap();
    assert_eq!(selected_area(&s), 1200.0, "together: the subtract shape cuts a hole");
    s.execute("roto.selection.make", json!({"each": true})).unwrap();
    assert_eq!(selected_area(&s), 1600.0, "each on its own, unioned: the hole is filled");
}

#[test]
fn selection_from_roto_rejects_bad_input_and_changes_nothing() {
    let mut s = with_roto_layer(100, 100);
    assert!(s.execute("roto.selection.make", json!({})).is_err(), "no shapes");
    let a = add_rect(&mut s, 0.0, 0.0, 20.0, 20.0);
    for params in [json!({"node": 999}), json!({"node": a, "mode": "xor"}), json!({"each": "yes"}), json!({"node": "a"})] {
        assert!(s.execute("roto.selection.make", params.clone()).is_err(), "{params}");
    }
    assert!(s.active().unwrap().doc.selection.is_none());
}

#[test]
fn a_selection_becomes_roto_shapes_that_reproduce_it_holes_included() {
    let mut s = session(100, 100);
    assert!(s.execute("roto.selection.to_shapes", json!({})).is_err(), "no selection");
    // A 60x60 square with a 20x20 hole, plus a separate 10x10 island.
    s.execute("select.rect", json!({"x": 10, "y": 10, "width": 60, "height": 60})).unwrap();
    s.execute("select.rect", json!({"x": 30, "y": 30, "width": 20, "height": 20, "mode": "subtract"})).unwrap();
    s.execute("select.rect", json!({"x": 80, "y": 80, "width": 10, "height": 10, "mode": "add"})).unwrap();
    let want = selection_values(&s);
    let r = s.execute("roto.selection.to_shapes", json!({})).unwrap();
    assert_eq!(r["shapes"].as_array().unwrap().len(), 3);
    assert!(r["group"].is_u64());
    let m = mask(&s);
    let got = vector::roto::roto_values(&m, Rect::new(0, 0, 100, 100));
    let diff: f64 = got.iter().zip(&want).map(|(a, b)| f64::from((a - b).abs())).sum();
    assert!(diff < 100.0, "the shapes follow the selection (diff {diff})");
    undo(&mut s);
    assert!(s.active().unwrap().doc.layer(s.active().unwrap().active_layer.unwrap()).unwrap().roto_mask.is_none(), "undo removes the mask it created");
}
