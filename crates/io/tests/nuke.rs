//! Nuke `.nk` Roto import/export against a real Roto node copied out of Nuke.

use photocraft_doc::RotoMask;
use photocraft_doc::roto::{Group, Node, Point, Shape, V2};
use photocraft_io::nuke::{NukeError, parse_nk, write_nk};

const SAMPLE: &str = include_str!("fixtures/nuke-roto-bezier-1.nk");
/// The format height of the sample (2048x1556).
const H: f64 = 1556.0;

fn shapes(m: &RotoMask) -> Vec<&Shape> {
    fn walk<'a>(g: &'a Group, out: &mut Vec<&'a Shape>) {
        for n in &g.children {
            match n {
                Node::Group(c) => walk(c, out),
                Node::Shape(s) => out.push(s),
            }
        }
    }
    let mut v = Vec::new();
    walk(&m.root, &mut v);
    v
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn parses_the_sample_bezier() {
    let (m, warnings) = parse_nk(SAMPLE, H).unwrap();
    let all = shapes(&m);
    assert_eq!(all.len(), 1);
    let s = all[0];
    assert_eq!(s.name, "Bezier1");
    assert_eq!(s.points.len(), 6);
    assert!(s.closed);
    // Nuke is y-up: (872, 1154) becomes y = 1556 - 1154 = 402 in document space.
    assert!(close(s.points[0].pos.x, 872.0) && close(s.points[0].pos.y, 402.0), "{:?}", s.points[0].pos);
    assert!(close(s.points[1].pos.x, 1242.0) && close(s.points[1].pos.y, 474.0));
    // The tangent pair (-196, 42) / (196, -42) flips its y sign.
    assert!(close(s.points[0].tangent_in.x, -196.0) && close(s.points[0].tangent_in.y, -42.0));
    assert!(close(s.points[0].tangent_out.x, 196.0) && close(s.points[0].tangent_out.y, 42.0));
    // Feather offsets: point 1 has (44, 116) in Nuke, (44, -116) here; points 3-5 have none; point 6 has (-128, 0).
    assert!(close(s.points[0].feather_pos.x, 44.0) && close(s.points[0].feather_pos.y, -116.0));
    assert_eq!(s.points[2].feather_pos, V2::ZERO);
    assert!(close(s.points[5].feather_pos.x, -128.0) && close(s.points[5].feather_pos.y, 0.0));
    // The sample's feather curve repeats the shape tangents, so the relative feather tangents are zero.
    assert!(s.points.iter().all(|p| close(p.feather_in.x, 0.0) && close(p.feather_in.y, 0.0) && close(p.feather_out.x, 0.0)));
    m.validate().unwrap();
    // UI-state attributes (osw, tt, ...) are not reported; the flags and transform pivot are known.
    assert!(warnings.iter().all(|w| !w.contains("osw") && !w.contains("tx")), "{warnings:?}");
}

#[test]
fn export_then_import_round_trips_geometry_and_feather() {
    let (m, _) = parse_nk(SAMPLE, H).unwrap();
    let text = write_nk(&m, 2048.0, H);
    assert!(text.starts_with("set cut_paste_input [stack 0]"), "{}", &text[..40.min(text.len())]);
    assert!(text.contains("Roto {") && text.contains("curvegroup Bezier1"));
    let (back, _) = parse_nk(&text, H).unwrap();
    let (a, b): (Vec<Point>, Vec<Point>) =
        (shapes(&m).iter().flat_map(|s| s.points.clone()).collect(), shapes(&back).iter().flat_map(|s| s.points.clone()).collect());
    assert_eq!(a.len(), b.len());
    for (p, q) in a.iter().zip(&b) {
        for (u, v) in [
            (p.pos, q.pos),
            (p.tangent_in, q.tangent_in),
            (p.tangent_out, q.tangent_out),
            (p.feather_pos, q.feather_pos),
            (p.feather_in, q.feather_in),
            (p.feather_out, q.feather_out),
        ] {
            assert!(close(u.x, v.x) && close(u.y, v.y), "{u:?} vs {v:?}");
        }
    }
    // Writing the re-imported mask again is stable (a fixed point).
    assert_eq!(write_nk(&back, 2048.0, H), text);
}

#[test]
fn exports_groups_and_several_shapes_and_reads_them_back() {
    let mut m = RotoMask::default();
    let mut g = Group::new(photocraft_doc::roto::NodeId(1), "Face");
    for (i, off) in [0.0, 50.0].into_iter().enumerate() {
        let mut s = Shape::new(photocraft_doc::roto::NodeId(2 + i as u64), &format!("Eye{i}"));
        for (k, (x, y)) in [(10.0, 10.0), (30.0, 10.0), (30.0, 30.0), (10.0, 30.0)].into_iter().enumerate() {
            let mut p = Point::corner(photocraft_doc::roto::PointId(10 * (i as u64 + 1) + k as u64), x + off, y);
            p.feather_pos = V2::new(2.0, -3.0);
            s.points.push(p);
        }
        g.children.push(Node::Shape(s));
    }
    m.root.children.push(Node::Group(g));
    let text = write_nk(&m, 200.0, 100.0);
    let (back, warnings) = parse_nk(&text, 100.0).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let Node::Group(face) = &back.root.children[0] else { panic!("group survives") };
    assert_eq!(face.name, "Face");
    let eyes: Vec<&Shape> = face.children.iter().filter_map(|n| if let Node::Shape(s) = n { Some(s) } else { None }).collect();
    assert_eq!(eyes.len(), 2);
    assert_eq!(eyes[1].name, "Eye1");
    assert!(close(eyes[1].points[0].pos.x, 60.0) && close(eyes[1].points[0].pos.y, 10.0));
    assert!(close(eyes[0].points[2].feather_pos.x, 2.0) && close(eyes[0].points[2].feather_pos.y, -3.0));
}

#[test]
fn unsupported_content_is_reported_not_fatal() {
    // A second, non-bezier shape in the same layer and an unknown attribute.
    let noisy = SAMPLE
        .replace("{a osw x41200000", "{a zzz 3 osw x41200000")
        .replace("     {tx 1 x44751555 x44736aab}", "     {tx 1 x44751555 x44736aab}\n     {stroke Brush1 7}");
    let (m, warnings) = parse_nk(&noisy, H).unwrap();
    assert_eq!(shapes(&m).len(), 1);
    assert!(warnings.iter().any(|w| w.contains("zzz")), "{warnings:?}");
    let spline = SAMPLE.replace("512 bezier", "512 bspline");
    let (m, warnings) = parse_nk(&spline, H).unwrap();
    assert!(shapes(&m).is_empty());
    assert!(warnings.iter().any(|w| w.contains("bspline")), "{warnings:?}");
}

#[test]
fn text_without_a_roto_node_and_bad_text_are_errors() {
    assert_eq!(parse_nk("", H).unwrap_err(), NukeError::NoRoto);
    assert_eq!(parse_nk("Blur { size 3 }", H).unwrap_err(), NukeError::NoRoto);
    assert!(matches!(parse_nk("Roto {", H), Err(NukeError::Syntax(_))));
    assert!(matches!(parse_nk("Roto { } }", H), Err(NukeError::Syntax(_))));
    // A point list that is not a whole number of triples.
    let broken = SAMPLE.replacen("{x42840000 xc1800000}\n", "", 1);
    assert!(matches!(parse_nk(&broken, H), Err(NukeError::Syntax(_))));
    assert!(parse_nk(SAMPLE, f64::NAN).is_err());
}

#[test]
fn hostile_text_never_panics_and_stays_bounded() {
    let deep = format!("Roto {}{}", "{".repeat(100_000), "}".repeat(100_000));
    let many = "a ".repeat(5_000_000);
    let huge = format!("Roto {{ curves {{{{ {} }}}} }}", "{x1 x2} ".repeat(300_000));
    let nasty_floats = SAMPLE.replace("x445a0000", "xZZZZ").replace("xc3440000", "x1234567890abcdef").replace("x42280000", "nan");
    let big = "x".repeat(40 << 20);
    for t in ["", "Roto", "Roto {", "{{{{", "}}}}", "Roto { curves {{x zz}} }", "\"unterminated", &deep, &many, &huge, &nasty_floats, &big] {
        let _ = parse_nk(t, 100.0);
    }
    assert_eq!(parse_nk(&big, 100.0).unwrap_err(), NukeError::TooLarge);
    // Non-finite and unparsable floats are replaced by zero and reported, never propagated.
    if let Ok((m, w)) = parse_nk(&nasty_floats, H) {
        m.validate().unwrap();
        assert!(!w.is_empty());
    }
}
