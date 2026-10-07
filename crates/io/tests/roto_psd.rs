//! Roto masks through document → PSD → document. Photoshop sees an ordinary raster layer mask
//! (the roto mask baked in); PhotoCraft also restores the editable splines from a private block
//! while the baked mask is still exactly what it exported.

mod common;

use common::*;
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::roto::{Node, NodeId, Point, PointId, Shape, V2};
use photocraft_doc::{Document, Layer, LayerMask, RotoMask, Size};
use photocraft_geom::Rect;
use photocraft_io::*;
use photocraft_psd::PsdFile;

const TOL: f32 = 1.0 / 255.0 + 1e-5;
const KEY: [u8; 4] = *b"PcRM";

fn roto(x0: f64, y0: f64, x1: f64, y1: f64) -> RotoMask {
    let mut s = Shape::new(NodeId(1), "Bezier1");
    for (i, (x, y)) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].into_iter().enumerate() {
        let mut p = Point::corner(PointId(i as u64 + 1), x, y);
        p.feather_pos = V2::new(if x == x0 { -4.0 } else { 4.0 }, if y == y0 { -4.0 } else { 4.0 });
        s.points.push(p);
    }
    s.opacity = 0.9;
    let mut m = RotoMask::default();
    m.root.children.push(Node::Shape(s));
    m
}

fn doc_with(r: Option<RotoMask>, mask: Option<LayerMask>) -> Document {
    let mut d = Document::new("t", Size::new(64, 48), ColorMode::Rgb, SampleType::U8);
    let mut l: Layer = raster("Roto layer", PixelFormat::RGBA8, Rect::new(0, 0, 64, 48), 7, false);
    l.roto_mask = r;
    l.mask = mask;
    d.layers.push(l);
    d
}

fn layer(d: &Document) -> &Layer {
    d.layers.iter().find(|l| l.name == "Roto layer").expect("layer survives")
}

fn export_bytes(d: &Document) -> Vec<u8> {
    export(d, "x.psd", &ExportOptions::default()).expect("export").bytes
}

fn import_doc(bytes: &[u8]) -> (Document, Vec<String>) {
    let r = import("x.psd", bytes).expect("import");
    (r.document, r.warnings)
}

/// What Photoshop (or any reader that ignores our private block) sees.
fn strip_block(bytes: &[u8]) -> Vec<u8> {
    let mut f = PsdFile::from_bytes(bytes).expect("parse");
    for rec in &mut f.layer_info.as_mut().expect("layers").layers {
        rec.blocks.retain(|b| b.key != KEY);
    }
    f.to_bytes().expect("write")
}

fn replace_block(bytes: &[u8], data: &[u8]) -> Vec<u8> {
    let mut f = PsdFile::from_bytes(bytes).expect("parse");
    for rec in &mut f.layer_info.as_mut().expect("layers").layers {
        for b in &mut rec.blocks {
            if b.key == KEY {
                b.data = data.to_vec();
                b.padding = None;
            }
        }
    }
    f.to_bytes().expect("write")
}

fn block_of(bytes: &[u8]) -> Vec<u8> {
    let f = PsdFile::from_bytes(bytes).expect("parse");
    f.layer_info.expect("layers").layers.iter().flat_map(|r| r.blocks.iter()).find(|b| b.key == KEY).map(|b| b.data.clone()).expect("PcRM block written")
}

fn assert_composite_eq(a: &Document, b: &Document, ctx: &str) {
    let (x, y) = (photocraft_compose::flatten(a), photocraft_compose::flatten(b));
    assert_eq!(x.rect, y.rect, "{ctx}: rect");
    let m = max_diff(&x.px, &y.px);
    assert!(m <= TOL, "{ctx}: composite differs by {m}");
}

#[test]
fn splines_survive_and_the_mask_is_not_double_applied() {
    let original = doc_with(Some(roto(10.0, 8.0, 40.0, 36.0)), None);
    let (back, warnings) = import_doc(&export_bytes(&original));
    assert!(warnings.iter().all(|w| !w.contains("roto")), "{warnings:?}");
    let l = layer(&back);
    assert_eq!(l.roto_mask, original.layers[0].roto_mask, "the editable splines come back");
    assert!(l.mask.is_none(), "the baked pixel mask is returned to its pre-bake state (none)");
    assert_composite_eq(&original, &back, "restored");
    // The roto mask really affects the image: the area outside the shape is transparent.
    let c = photocraft_compose::flatten(&back);
    let at = |x: i32, y: i32| c.px[((y - c.rect.y0) as usize) * c.rect.width() as usize + (x - c.rect.x0) as usize][3];
    assert!(at(25, 20) > 0.8 && at(2, 2) < 0.01);
}

#[test]
fn photoshop_sees_the_roto_mask_as_a_plain_layer_mask() {
    let original = doc_with(Some(roto(10.0, 8.0, 40.0, 36.0)), None);
    let bytes = strip_block(&export_bytes(&original));
    let (seen, _) = import_doc(&bytes);
    let l = layer(&seen);
    assert!(l.roto_mask.is_none() && l.mask.is_some(), "an ordinary raster layer mask");
    assert_composite_eq(&original, &seen, "what Photoshop shows");
}

#[test]
fn an_existing_pixel_mask_is_multiplied_in_and_restored_exactly() {
    let mut pm = LayerMask::reveal_all();
    pm.surface.fill_rect(Rect::new(0, 0, 64, 20), &[0.0]);
    pm.density = 0.8;
    let original = doc_with(Some(roto(10.0, 8.0, 40.0, 36.0)), Some(pm.clone()));
    let bytes = export_bytes(&original);
    let (back, _) = import_doc(&bytes);
    let l = layer(&back);
    assert_eq!(l.roto_mask, original.layers[0].roto_mask);
    let got = l.mask.as_ref().expect("the pixel mask comes back");
    assert!((got.density - 0.8).abs() < 0.01 && got.feather == 0.0);
    let area = Rect::new(0, 0, 64, 48);
    let (want, have) = (pm.surface.read_region(area), got.surface.read_region(area));
    assert!(want.iter().zip(&have).all(|(a, b)| (a - b).abs() <= TOL), "pre-bake pixel mask restored");
    assert_composite_eq(&original, &back, "pixel mask + roto restored");
    // Photoshop's view: one mask, the product of both.
    let (seen, _) = import_doc(&strip_block(&bytes));
    assert_composite_eq(&original, &seen, "product of both masks");
}

#[test]
fn a_disabled_roto_mask_round_trips_without_masking_anything() {
    let mut r = roto(10.0, 8.0, 40.0, 36.0);
    r.enabled = false;
    let original = doc_with(Some(r), None);
    let (back, _) = import_doc(&export_bytes(&original));
    let l = layer(&back);
    assert_eq!(l.roto_mask.as_ref().map(|m| m.enabled), Some(false));
    assert!(l.mask.is_none());
    assert_composite_eq(&original, &back, "disabled");
    let (seen, _) = import_doc(&strip_block(&export_bytes(&original)));
    assert!(layer(&seen).mask.is_none(), "nothing is baked for a disabled mask");
}

#[test]
fn a_mask_changed_elsewhere_is_kept_as_a_plain_mask() {
    let a = export_bytes(&doc_with(Some(roto(10.0, 8.0, 40.0, 36.0)), None));
    let b = export_bytes(&doc_with(Some(roto(20.0, 10.0, 60.0, 40.0)), None));
    // File A's baked mask with file B's splines: the hash no longer matches.
    let franken = replace_block(&a, &block_of(&b));
    let (back, warnings) = import_doc(&franken);
    let l = layer(&back);
    assert!(l.roto_mask.is_none() && l.mask.is_some());
    assert!(warnings.iter().any(|w| w.contains("roto") && w.contains("Roto layer")), "{warnings:?}");
    let (plain, _) = import_doc(&strip_block(&a));
    assert_composite_eq(&back, &plain, "kept exactly as Photoshop would show it");
}

#[test]
fn hostile_block_payloads_never_panic_and_keep_the_baked_mask() {
    let bytes = export_bytes(&doc_with(Some(roto(10.0, 8.0, 40.0, 36.0)), None));
    let good = block_of(&bytes);
    let mut bad_version = good.clone();
    bad_version[0] = 99;
    let mut huge_len = good.clone();
    huge_len[1..5].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut garbage_json = good.clone();
    for b in garbage_json.iter_mut().skip(5).take(20) {
        *b = 0xff;
    }
    let cases: Vec<Vec<u8>> = vec![vec![], vec![1], vec![1, 0, 0, 0, 0], vec![9; 100], good[..good.len() / 2].to_vec(), bad_version, huge_len, garbage_json];
    for (i, payload) in cases.iter().enumerate() {
        let (back, _) = import_doc(&replace_block(&bytes, payload));
        let l = layer(&back);
        assert!(l.roto_mask.is_none() && l.mask.is_some(), "case {i}: baked mask kept");
    }
}

#[test]
fn layers_without_a_roto_mask_write_no_block() {
    let plain = export_bytes(&doc_with(None, None));
    let f = PsdFile::from_bytes(&plain).expect("parse");
    assert!(f.layer_info.expect("layers").layers.iter().all(|r| r.blocks.iter().all(|b| b.key != KEY)));
    // Importing and re-exporting a file never carries a stale block along.
    let with = export_bytes(&doc_with(Some(roto(10.0, 8.0, 40.0, 36.0)), None));
    let (mut back, _) = import_doc(&with);
    back.layers[0].roto_mask = None;
    let again = PsdFile::from_bytes(&export_bytes(&back)).expect("parse");
    assert!(again.layer_info.expect("layers").layers.iter().all(|r| r.blocks.iter().all(|b| b.key != KEY)));
}
