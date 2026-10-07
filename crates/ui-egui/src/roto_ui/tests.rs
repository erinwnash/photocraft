use photocraft_doc::roto::{Node, V2};
use photocraft_vector as vector;
use serde_json::json;

use super::*;
use crate::Services;

fn app() -> PhotocraftApp {
    let mut a = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    a.run("file.new", json!({"width": 300, "height": 300, "background": "white", "depth": 8})).unwrap();
    // The Background layer is position-locked; draw on a regular layer.
    a.run("layer.new.layer", json!({"name": "Roto layer"})).unwrap();
    a.ui.tool = Tool::Roto;
    a
}

fn mask(a: &PhotocraftApp) -> RotoMask {
    active(a).map(|(_, m)| m.clone()).expect("the layer has a roto mask")
}

fn shapes(a: &PhotocraftApp) -> Vec<photocraft_doc::roto::Shape> {
    mask(a).root.children.iter().filter_map(|n| if let Node::Shape(s) = n { Some(s.clone()) } else { None }).collect()
}

fn click(a: &mut PhotocraftApp, p: P2, mods: Modifiers) {
    down(a, p[0], p[1], mods);
    up(a, p[0], p[1]);
}

fn drag(a: &mut PhotocraftApp, from: P2, to: P2, mods: Modifiers) {
    down(a, from[0], from[1], mods);
    let steps = 4;
    for i in 1..=steps {
        let t = f64::from(i) / f64::from(steps);
        moved(a, from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t);
    }
    up(a, to[0], to[1]);
}

fn undo(a: &mut PhotocraftApp) {
    a.run("edit.undo", json!({})).unwrap();
}

fn pos(a: &PhotocraftApp, shape: usize, point: usize) -> P2 {
    let s = &shapes(a)[shape];
    [s.points[point].pos.x, s.points[point].pos.y]
}

#[test]
fn rectangle_and_ellipse_modes_create_shapes_and_select_their_points() {
    let mut a = app();
    a.ui.roto.mode = RotoMode::Rectangle;
    drag(&mut a, [20.0, 30.0], [100.0, 90.0], Modifiers::NONE);
    let s = shapes(&a);
    assert_eq!(s.len(), 1, "the roto mask was created on demand");
    assert_eq!((s[0].points.len(), pos(&a, 0, 0), pos(&a, 0, 2)), (4, [20.0, 30.0], [100.0, 90.0]));
    assert_eq!((a.ui.roto.sel.shape, a.ui.roto.sel.points.len()), (Some(s[0].id), 4));

    a.ui.roto.mode = RotoMode::Ellipse;
    drag(&mut a, [150.0, 150.0], [250.0, 220.0], Modifiers::NONE);
    assert_eq!(shapes(&a).len(), 2);
    assert!(shapes(&a)[1].points.iter().all(|p| p.smooth));
    // A drag too small to be a shape makes nothing.
    drag(&mut a, [10.0, 10.0], [11.0, 11.0], Modifiers::NONE);
    assert_eq!(shapes(&a).len(), 2);
}

#[test]
fn freehand_mode_fits_the_stroke_to_bezier_points() {
    let mut a = app();
    a.ui.roto.mode = RotoMode::Freehand;
    let pts: Vec<P2> = (0..60)
        .map(|i| {
            let t = f64::from(i) / 60.0 * std::f64::consts::TAU;
            [150.0 + 60.0 * t.cos(), 150.0 + 60.0 * t.sin()]
        })
        .collect();
    down(&mut a, pts[0][0], pts[0][1], Modifiers::NONE);
    for p in &pts[1..] {
        moved(&mut a, p[0], p[1]);
    }
    assert!(matches!(a.ui.roto.gesture, Some(Gesture::Freehand(_))), "the stroke is in progress");
    up(&mut a, pts[59][0], pts[59][1]);
    let s = shapes(&a);
    assert_eq!(s.len(), 1);
    assert!(s[0].points.len() >= 2 && s[0].points.len() < 30, "{} points", s[0].points.len());
    // The fit reproduces the stroke: the mask covers a disc of radius 60 within a few percent.
    let area: f64 = photocraft_vector::roto::roto_values(&mask(&a), photocraft_geom::Rect::new(0, 0, 300, 300)).iter().map(|v| f64::from(*v)).sum();
    let want = std::f64::consts::PI * 60.0 * 60.0;
    assert!((area - want).abs() / want < 0.04, "area {area} vs {want}");
    assert!(a.ui.roto.gesture.is_none());
    // A single click is not a stroke.
    click(&mut a, [10.0, 10.0], Modifiers::NONE);
    assert_eq!(shapes(&a).len(), 1);
}

fn with_rect() -> PhotocraftApp {
    let mut a = app();
    a.ui.roto.mode = RotoMode::Rectangle;
    drag(&mut a, [20.0, 20.0], [100.0, 80.0], Modifiers::NONE);
    a.ui.roto.mode = RotoMode::Select;
    a.ui.roto.sel.clear();
    a
}

#[test]
fn clicking_selects_shift_toggles_and_dragging_moves_in_one_undo_step() {
    let mut a = with_rect();
    click(&mut a, [20.0, 20.0], Modifiers::NONE);
    assert_eq!(a.ui.roto.sel.points.len(), 1);
    click(&mut a, [100.0, 20.0], Modifiers::SHIFT);
    assert_eq!(a.ui.roto.sel.points.len(), 2, "shift adds");
    click(&mut a, [100.0, 20.0], Modifiers::SHIFT);
    assert_eq!(a.ui.roto.sel.points.len(), 1, "shift toggles off");

    // Dragging a selected point moves it (and only the selection).
    drag(&mut a, [20.0, 20.0], [35.0, 28.0], Modifiers::NONE);
    assert_eq!(pos(&a, 0, 0), [35.0, 28.0]);
    assert_eq!(pos(&a, 0, 1), [100.0, 20.0]);
    // Every mouse move was coalesced: one undo restores the start.
    undo(&mut a);
    assert_eq!(pos(&a, 0, 0), [20.0, 20.0]);

    // Dragging an unselected point selects it first, then moves it.
    drag(&mut a, [100.0, 80.0], [110.0, 90.0], Modifiers::NONE);
    assert_eq!(pos(&a, 0, 2), [110.0, 90.0]);
    assert_eq!(a.ui.roto.sel.points.len(), 1);
}

#[test]
fn dragging_a_selected_group_of_points_moves_them_together() {
    let mut a = with_rect();
    a.ui.roto.sel.set(shapes(&a)[0].id, shapes(&a)[0].points.iter().map(|p| p.id).collect(), false);
    drag(&mut a, [20.0, 20.0], [30.0, 25.0], Modifiers::NONE);
    assert_eq!((pos(&a, 0, 0), pos(&a, 0, 2)), ([30.0, 25.0], [110.0, 85.0]), "all four moved by (10, 5)");
}

#[test]
fn marquee_selects_the_points_inside_and_a_click_on_nothing_clears() {
    let mut a = with_rect();
    drag(&mut a, [10.0, 10.0], [110.0, 30.0], Modifiers::NONE);
    assert_eq!(a.ui.roto.sel.points.len(), 2, "the top two corners");
    click(&mut a, [250.0, 250.0], Modifiers::NONE);
    assert_eq!(a.ui.roto.sel, Selection::default());
    // Clicking the outline selects the whole shape.
    click(&mut a, [60.0, 20.0], Modifiers::NONE);
    assert_eq!(a.ui.roto.sel.points.len(), 4);
}

#[test]
fn handles_bend_feather_and_insert_points() {
    let mut a = with_rect();
    click(&mut a, [100.0, 20.0], Modifiers::NONE);
    let id = shapes(&a)[0].points[1].id;
    // Command-drag on the point pulls its soft edge out.
    drag(&mut a, [100.0, 20.0], [112.0, 8.0], Modifiers::COMMAND);
    let f = shapes(&a)[0].points[1].feather_pos;
    assert_eq!((f.x, f.y), (12.0, -12.0));
    // The feather handle is now draggable on its own.
    drag(&mut a, [112.0, 8.0], [120.0, 4.0], Modifiers::NONE);
    let f = shapes(&a)[0].points[1].feather_pos;
    assert_eq!((f.x, f.y), (20.0, -16.0));
    // Give the point a tangent, then drag its tip.
    a.run("roto.point.set", json!({"shape": shapes(&a)[0].id.0, "id": id.0, "out": [0.0, 15.0]})).unwrap();
    drag(&mut a, [100.0, 35.0], [100.0, 45.0], Modifiers::NONE);
    assert_eq!(shapes(&a)[0].points[1].tangent_out.y, 25.0);
    // Command+Option click on the outline inserts a point there.
    let before = shapes(&a)[0].points.len();
    click(&mut a, [60.0, 80.0], Modifiers { command: true, alt: true, ..Modifiers::NONE });
    assert_eq!(shapes(&a)[0].points.len(), before + 1);
}

#[test]
fn pen_mode_builds_a_shape_with_smooth_handles_and_closes_on_the_first_point() {
    let mut a = app();
    a.ui.roto.mode = RotoMode::Pen;
    click(&mut a, [50.0, 50.0], Modifiers::NONE);
    assert!(a.ui.roto.drawing.is_some());
    // Click and drag: the second point gets smooth handles.
    drag(&mut a, [150.0, 50.0], [170.0, 50.0], Modifiers::NONE);
    let p = shapes(&a)[0].points[1];
    assert!(p.smooth);
    assert_eq!((p.tangent_out.x, p.tangent_out.y, p.tangent_in.x, p.tangent_in.y), (20.0, 0.0, -20.0, 0.0));
    click(&mut a, [150.0, 150.0], Modifiers::NONE);
    assert_eq!(shapes(&a)[0].points.len(), 3);
    // Clicking the first point closes the shape and ends drawing; no extra point is added.
    click(&mut a, [50.0, 50.0], Modifiers::NONE);
    assert_eq!(shapes(&a)[0].points.len(), 3);
    assert!(a.ui.roto.drawing.is_none());
    assert_eq!(a.ui.roto.sel.points.len(), 3);
    // The next click starts a new shape.
    click(&mut a, [200.0, 200.0], Modifiers::NONE);
    assert_eq!(shapes(&a).len(), 2);
}

fn press(a: &mut PhotocraftApp, key: egui::Key, modifiers: Modifiers) -> bool {
    let ctx = egui::Context::default();
    let mut raw = egui::RawInput::default();
    raw.events.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
    let mut used = false;
    let mut out = ctx.run_ui(raw, |ui| used = keys(a, ui.ctx()));
    out.textures_delta.clear();
    used
}

#[test]
fn keys_nudge_delete_and_finish_drawing() {
    let mut a = with_rect();
    assert!(!press(&mut a, egui::Key::Delete, Modifiers::NONE), "nothing selected: the key is not ours");
    click(&mut a, [20.0, 20.0], Modifiers::NONE);
    assert!(press(&mut a, egui::Key::ArrowRight, Modifiers::NONE));
    assert!(press(&mut a, egui::Key::ArrowDown, Modifiers::SHIFT));
    assert_eq!(pos(&a, 0, 0), [21.0, 30.0]);
    assert!(press(&mut a, egui::Key::Delete, Modifiers::NONE));
    assert_eq!(shapes(&a)[0].points.len(), 3);
    assert_eq!(a.ui.roto.sel, Selection::default(), "deleted points leave the selection");
    // Other tools never lose their keys to the roto tool.
    a.ui.tool = Tool::Brush;
    click(&mut a, [100.0, 20.0], Modifiers::NONE);
    a.ui.tool = Tool::Brush;
    assert!(!press(&mut a, egui::Key::Delete, Modifiers::NONE));

    // Enter / Escape end a shape in progress.
    a.ui.tool = Tool::Roto;
    a.ui.roto.mode = RotoMode::Pen;
    click(&mut a, [200.0, 200.0], Modifiers::NONE);
    assert!(a.ui.roto.drawing.is_some());
    assert!(press(&mut a, egui::Key::Enter, Modifiers::NONE));
    assert!(a.ui.roto.drawing.is_none());
}

#[test]
fn a_locked_or_hidden_shape_cannot_be_picked_and_no_mask_means_no_edit() {
    let mut a = app();
    a.ui.roto.mode = RotoMode::Select;
    // No roto mask yet: clicks do nothing and create nothing.
    click(&mut a, [10.0, 10.0], Modifiers::NONE);
    assert!(active(&a).is_none());
    let mut a = with_rect();
    let id = shapes(&a)[0].id;
    a.run("roto.node.set", json!({"id": id.0, "locked": true})).unwrap();
    click(&mut a, [20.0, 20.0], Modifiers::NONE);
    assert_eq!(a.ui.roto.sel, Selection::default());
}

#[test]
fn panel_and_overlay_render_with_groups_selection_and_gestures() {
    let mut a = with_rect();
    a.run("roto.feather.set_all", json!({"distance": 6.0})).unwrap();
    let ids: Vec<u64> = vec![shapes(&a)[0].id.0];
    a.run("roto.node.group", json!({"ids": ids})).unwrap();
    a.run("roto.node.add_ellipse", json!({"rect": [150, 150, 60, 40]})).unwrap();
    a.ui.roto.sel.set(shapes(&a)[0].id, vec![], false);
    click(&mut a, [150.0, 170.0], Modifiers::NONE);
    a.ui.roto.nodes = vec![mask(&a).root.children.iter().filter_map(|n| if let Node::Shape(s) = n { Some(s.id) } else { None }).next().unwrap_or_default()];
    a.ui.roto.gesture = Some(Gesture::Marquee { from: [0.0, 0.0], to: [50.0, 50.0], additive: false });
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    for _ in 0..3 {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            panel(&mut a, ui);
            let xf = ViewXform { rect: ui.max_rect(), zoom: 1.0, center: [150.0, 150.0], flip: false };
            draw_overlay(&a, ui.painter(), &xf);
            assert!(options_bar(&mut a, ui, Tool::Roto));
            assert!(!options_bar(&mut a, ui, Tool::Brush));
        });
        out.textures_delta.clear();
    }
    // The panel for a layer with no roto mask offers to add one, and survives a frame.
    let mut b = app();
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| panel(&mut b, ui));
    out.textures_delta.clear();
    assert!(active(&b).is_none());
}

/// The canvas hands tool events to the roto tool (not just `roto_ui::down` called directly).
#[test]
fn canvas_tool_events_reach_the_roto_tool() {
    use crate::canvas::{ToolEvent, tool_event};
    let mut a = app();
    a.ui.roto.mode = RotoMode::Rectangle;
    tool_event(&mut a, ToolEvent::Down { x: 40.0, y: 40.0, pressure: 1.0 }, Modifiers::NONE);
    tool_event(&mut a, ToolEvent::Move { x: 90.0, y: 80.0, pressure: 1.0 }, Modifiers::NONE);
    tool_event(&mut a, ToolEvent::Up { x: 90.0, y: 80.0 }, Modifiers::NONE);
    assert_eq!(shapes(&a).len(), 1);
    assert_eq!(pos(&a, 0, 2), [90.0, 80.0]);
    assert!(a.drag.is_none(), "no paint or marquee drag was started on the side");
}

#[test]
fn mode_names_round_trip_and_ui_state_serializes_without_transient_fields() {
    for m in RotoMode::ALL {
        assert_eq!(RotoMode::from_name(m.name()), Some(m));
    }
    assert_eq!(RotoMode::from_name("nope"), None);
    let mut ui = RotoUi { mode: RotoMode::Pen, hide_feather: true, ..Default::default() };
    ui.sel.set(NodeId(1), vec![PointId(2)], false);
    ui.nuke_text = "secret".into();
    let json = serde_json::to_value(&ui).unwrap();
    assert_eq!(json["mode"], "Pen");
    assert!(json.get("sel").is_none() && json.get("nuke_text").is_none());
    let back: RotoUi = serde_json::from_value(json).unwrap();
    assert_eq!((back.mode, back.hide_feather, back.sel), (RotoMode::Pen, true, Selection::default()));
}

// ---------------------------------------------------------------------------------------------
// The editing view: the image stays visible while a roto mask is edited, the mask drawn over it.

/// The editing layer is process-wide: tests that touch it run one at a time.
static VIEW_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn editing() -> Option<photocraft_doc::LayerId> {
    vector::roto::editing_layer()
}

fn frame(a: &mut PhotocraftApp, ctx: &egui::Context) {
    let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
    out.textures_delta.clear();
    sync_view(a, ctx);
}

#[test]
fn the_image_stays_visible_while_editing_and_the_overlay_is_optional() {
    let _g = VIEW_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    vector::roto::set_editing_layer(None);
    let ctx = egui::Context::default();
    let mut a = app();
    let layer = a.session.active().unwrap().active_layer.unwrap();
    assert!(!a.ui.roto.matte_overlay && !a.ui.roto.apply_while_editing, "both off by default");
    // Drawing the first points: the mask is not applied, so the image never disappears.
    a.ui.roto.mode = RotoMode::Pen;
    click(&mut a, [40.0, 40.0], Modifiers::NONE);
    frame(&mut a, &ctx);
    assert_eq!(editing(), Some(layer));
    assert!(a.ui.roto.matte.tex.is_none(), "no red overlay unless asked for");
    // Tick "Show mask overlay": the mask is drawn over the image.
    a.ui.roto.matte_overlay = true;
    frame(&mut a, &ctx);
    assert_eq!(editing(), Some(layer));
    assert!(a.ui.roto.matte.tex.is_some());
    // Another tool: the mask applies to the layer again.
    a.ui.tool = Tool::Brush;
    frame(&mut a, &ctx);
    assert_eq!(editing(), None);
    assert!(a.ui.roto.matte.tex.is_none(), "no overlay texture is kept around");
    a.ui.tool = Tool::Roto;
    frame(&mut a, &ctx);
    assert_eq!(editing(), Some(layer));
    // "Apply mask while editing" applies it live.
    a.ui.roto.apply_while_editing = true;
    frame(&mut a, &ctx);
    assert_eq!(editing(), None);
    a.ui.roto.apply_while_editing = false;
    // A disabled mask is not applied, so there is nothing to see through.
    a.run("roto.instance.set", json!({"enabled": false})).unwrap();
    frame(&mut a, &ctx);
    assert_eq!(editing(), None);
    a.run("roto.instance.set", json!({"enabled": true})).unwrap();
    frame(&mut a, &ctx);
    assert_eq!(editing(), Some(layer));
    // The layer losing its mask must not leave the view stuck on or loop.
    a.run("layer.roto.delete", json!({})).unwrap();
    frame(&mut a, &ctx);
    frame(&mut a, &ctx);
    assert_eq!(editing(), None);
    vector::roto::set_editing_layer(None);
}

#[test]
fn switching_documents_ends_the_editing_view_but_never_touches_a_view_this_app_does_not_own() {
    let _g = VIEW_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    vector::roto::set_editing_layer(None);
    let ctx = egui::Context::default();
    let mut a = app();
    a.ui.roto.matte_overlay = true;
    a.ui.roto.mode = RotoMode::Rectangle;
    drag(&mut a, [40.0, 40.0], [120.0, 100.0], Modifiers::NONE);
    frame(&mut a, &ctx);
    let first = editing().expect("editing the first document's layer");
    // A new document becomes active: the first document's layer must not stay in the editing view.
    a.run("file.new", json!({"width": 100, "height": 100, "background": "white", "depth": 8})).unwrap();
    a.run("layer.new.layer", json!({"name": "Other"})).unwrap();
    frame(&mut a, &ctx);
    assert_ne!(editing(), Some(first));
    assert_eq!(editing(), None, "nothing is edited in the new document");

    // Another app instance (or test) owns a view: running a frame here must leave it alone.
    let theirs = photocraft_doc::LayerId(987_654);
    vector::roto::set_editing_layer(Some(theirs));
    a.ui.tool = Tool::Roto;
    frame(&mut a, &ctx);
    a.ui.tool = Tool::Brush;
    frame(&mut a, &ctx);
    assert_eq!(editing(), Some(theirs), "a view this app did not start is not this app's to end");
    vector::roto::set_editing_layer(None);
}

#[test]
fn the_matte_is_red_over_hidden_areas_and_clear_over_revealed_ones() {
    let mut m = RotoMask::default();
    let mut s = photocraft_doc::roto::Shape::new(NodeId(1), "s");
    for (i, (x, y)) in [(10.0, 10.0), (30.0, 10.0), (30.0, 30.0), (10.0, 30.0)].into_iter().enumerate() {
        s.points.push(photocraft_doc::roto::Point::corner(PointId(i as u64 + 1), x, y));
    }
    m.root.children.push(Node::Shape(s));
    let ([w, h], px) = matte_image(&m, 40, 40, 1024);
    assert_eq!((w, h, px.len()), (40, 40, 1600), "small documents are shown at full size");
    let at = |x: usize, y: usize| px[y * w + x];
    assert_eq!(at(20, 20).a(), 0, "inside the shape the image shows unchanged");
    let hidden = at(2, 2);
    assert!(hidden.a() >= 126 && hidden.a() <= 129, "half-strength rubylith where the mask hides: {hidden:?}");
    assert!(hidden.r() > hidden.g() && hidden.r() > hidden.b(), "and it is red");
    // Big documents are previewed at reduced size, with the mask scaled to match.
    let ([w, h], px) = matte_image(&m, 40, 40, 20);
    assert_eq!((w, h), (20, 20));
    assert_eq!(px[10 * 20 + 10].a(), 0);
    assert!(px[20 + 1].a() > 100);
}

#[test]
fn a_reduced_matte_scales_blur_and_root_moves_so_it_matches_the_full_size_mask() {
    let mut m = RotoMask::default();
    let mut s = photocraft_doc::roto::Shape::new(NodeId(1), "s");
    for (i, (x, y)) in [(40.0, 40.0), (120.0, 40.0), (120.0, 120.0), (40.0, 120.0)].into_iter().enumerate() {
        s.points.push(photocraft_doc::roto::Point::corner(PointId(i as u64 + 1), x, y));
    }
    s.blur = 12.0;
    m.root.children.push(Node::Shape(s));
    // A linked layer move rides on the root transform, which the preview must scale too.
    m.root.transform.translate = photocraft_doc::roto::V2::new(10.0, 6.0);
    let full = vector::roto::roto_values(&m, photocraft_geom::Rect::new(0, 0, 200, 200));
    let ([w, _], px) = matte_image(&m, 200, 200, 100);
    assert_eq!(w, 100);
    let mut worst = 0.0f32;
    for y in 0..100usize {
        for x in 0..100usize {
            // Matte alpha is 0.5 * (1 - coverage); compare it with the full-size mask at the same spot.
            let preview_cov = 1.0 - f32::from(px[y * 100 + x].a()) / 127.5;
            let full_cov = full[(y * 2 + 1) * 200 + (x * 2 + 1)];
            worst = worst.max((preview_cov - full_cov).abs());
        }
    }
    assert!(worst < 0.12, "preview and full-size masks differ by {worst}");
}

// ---------------------------------------------------------------------------------------------
// Cusp, uncusp and smooth

const POINT_COMMANDS: [&str; 3] = ["roto.cuspPoints", "roto.uncuspPoints", "roto.smoothPoints"];

fn handles_of(a: &PhotocraftApp, shape: usize, point: usize) -> (V2, V2, bool) {
    let p = shapes(a)[shape].points[point];
    (p.tangent_in, p.tangent_out, p.smooth)
}

#[test]
fn point_commands_act_on_the_selected_points_through_the_menu() {
    let ctx = egui::Context::default();
    let mut a = with_rect();
    // No shape at all: disabled, and invoking says why without touching anything.
    let mut empty = app();
    for id in POINT_COMMANDS {
        assert!(!crate::menus::is_enabled(&empty, id), "{id}");
        assert!(crate::menus::invoke(&mut empty, &ctx, id, json!({})).is_err(), "{id}");
    }
    // Nothing selected but a shape exists: it is acted on whole.
    assert!(crate::menus::is_enabled(&a, "roto.cuspPoints"));

    // Two points selected: Smooth builds their handles and leaves the others alone.
    click(&mut a, [20.0, 20.0], Modifiers::NONE);
    click(&mut a, [100.0, 20.0], Modifiers::SHIFT);
    assert_eq!(a.ui.roto.sel.points.len(), 2);
    for id in POINT_COMMANDS {
        assert!(crate::menus::is_enabled(&a, id), "{id}");
    }
    crate::menus::invoke(&mut a, &ctx, "roto.smoothPoints", json!({})).unwrap();
    let (tin, tout, smooth) = handles_of(&a, 0, 0);
    assert!(smooth && tout != V2::ZERO && tin == V2::new(-tout.x, -tout.y), "{tin:?} {tout:?}");
    assert!(handles_of(&a, 0, 1).2);
    assert_eq!(handles_of(&a, 0, 2), (V2::ZERO, V2::ZERO, false), "an unselected point is untouched");

    // Cusp leaves the handles where they are and breaks the link; Uncusp links them again.
    crate::menus::invoke(&mut a, &ctx, "roto.cuspPoints", json!({})).unwrap();
    assert_eq!(handles_of(&a, 0, 0), (tin, tout, false));
    crate::menus::invoke(&mut a, &ctx, "roto.uncuspPoints", json!({})).unwrap();
    assert_eq!(handles_of(&a, 0, 0), (tin, tout, true), "already collinear: linking changes nothing but the flag");

    // With no points selected, a shape selected in the Roto panel is acted on as a whole.
    a.ui.roto.sel.clear();
    a.ui.roto.nodes = vec![shapes(&a)[0].id];
    assert!(crate::menus::is_enabled(&a, "roto.smoothPoints"));
    crate::menus::invoke(&mut a, &ctx, "roto.smoothPoints", json!({})).unwrap();
    assert!(shapes(&a)[0].points.iter().all(|p| p.smooth && p.tangent_out != V2::ZERO));
    // Each press was one undo step.
    undo(&mut a);
    assert!(!handles_of(&a, 0, 2).2);
}

#[test]
fn the_point_commands_are_menu_items_users_can_map_keys_to() {
    let a = app();
    let items = crate::menus::menu_items(&a);
    for (id, label) in [("roto.cuspPoints", "Cusp Points"), ("roto.uncuspPoints", "Uncusp Points"), ("roto.smoothPoints", "Smooth Points")] {
        let item = items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("{id} is a menu item"));
        assert_eq!((item.label.as_str(), item.path.as_slice()), (label, &["Layer".to_string(), "Roto Mask".to_string()][..]));
        assert_eq!(item.shortcut, None, "no default key: the user assigns one");
        assert!(crate::menus::is_live(id));
        assert!(crate::roto_ui::handles(id));
    }
}

#[test]
fn a_hotkey_the_user_assigns_runs_the_point_command() {
    let ctx = egui::Context::default();
    let mut a = with_rect();
    click(&mut a, [20.0, 20.0], Modifiers::NONE);
    // Unassigned, the key does nothing.
    let press = |a: &mut PhotocraftApp| {
        let mods = Modifiers { command: true, alt: true, shift: true, ctrl: false, mac_cmd: false };
        let raw = egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(mods),
                egui::Event::Key { key: egui::Key::Num7, physical_key: None, pressed: true, repeat: false, modifiers: mods },
            ],
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| crate::shortcuts::handle(a, ui.ctx()));
        out.textures_delta.clear();
    };
    press(&mut a);
    assert!(!handles_of(&a, 0, 0).2, "no binding yet");
    // Assign Cmd+Alt+Shift+7 to Smooth Points (what Edit > Keyboard Shortcuts stores).
    a.session.prefs.edit(|p| p.shortcuts.insert("roto.smoothPoints".into(), "Cmd+Alt+Shift+7".into()));
    assert!(crate::shortcut_dispatch::bindings(&a).iter().any(|(id, _)| id == "roto.smoothPoints"));
    press(&mut a);
    assert!(handles_of(&a, 0, 0).2, "the mapped key smoothed the selected point");
    assert_ne!(handles_of(&a, 0, 0).1, V2::ZERO);
    // The tooltip on the button names the key.
    assert!(crate::shortcuts::tip_label(&a, "Smooth", "roto.smoothPoints").contains(&crate::shortcuts::pretty("Cmd+Alt+Shift+7")));
}

#[test]
fn the_options_bar_has_the_point_buttons_and_the_overlay_checkbox_unticked() {
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let mut a = with_rect();
    click(&mut a, [20.0, 20.0], Modifiers::NONE);
    for _ in 0..3 {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(options_bar(&mut a, ui, Tool::Roto));
        });
        out.textures_delta.clear();
    }
    assert!(!a.ui.roto.matte_overlay);
    assert_eq!(point_texts("roto.cuspPoints").0, "Cusp");
    assert_eq!(point_texts("roto.uncuspPoints").0, "Uncusp");
    assert_eq!(point_texts("roto.smoothPoints").0, "Smooth");
}

#[test]
fn point_buttons_act_on_the_whole_shape_when_no_points_are_selected() {
    let mut a = app();
    a.ui.roto.mode = RotoMode::Pen;
    for p in [[40.0, 40.0], [160.0, 40.0], [160.0, 160.0], [40.0, 160.0]] {
        click(&mut a, p, Modifiers::NONE);
    }
    // Still drawing: only the last point is selected, so only it is smoothed.
    menu(&mut a, "roto.smoothPoints", &json!({})).unwrap().unwrap();
    assert_eq!(shapes(&a)[0].points.iter().filter(|p| p.smooth).count(), 1);
    // Nothing selected (Select mode, empty selection): the newest shape, whole.
    a.ui.roto.mode = RotoMode::Select;
    a.ui.roto.drawing = None;
    a.ui.roto.sel.clear();
    assert_eq!(is_enabled(&a, "roto.cuspPoints"), Some(true));
    menu(&mut a, "roto.cuspPoints", &json!({})).unwrap().unwrap();
    assert!(shapes(&a)[0].points.iter().all(|p| !p.smooth));
    menu(&mut a, "roto.smoothPoints", &json!({})).unwrap().unwrap();
    assert!(shapes(&a)[0].points.iter().all(|p| p.smooth && (p.tangent_out != V2::ZERO || p.tangent_in != V2::ZERO)));
}

#[test]
fn enter_finishes_the_pen_shape_and_selects_all_its_points() {
    let ctx = egui::Context::default();
    let mut a = app();
    a.ui.roto.mode = RotoMode::Pen;
    for p in [[40.0, 40.0], [160.0, 40.0], [160.0, 160.0]] {
        click(&mut a, p, Modifiers::NONE);
    }
    let raw = egui::RawInput {
        events: vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }],
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| assert!(keys(&mut a, ui.ctx())));
    out.textures_delta.clear();
    assert_eq!(a.ui.roto.drawing, None);
    assert_eq!(a.ui.roto.sel.points.len(), 3);
}

#[test]
fn clicking_the_smooth_button_smooths_the_selected_point() {
    use egui_kittest::kittest::Queryable;
    let mut a = with_rect();
    click(&mut a, [20.0, 20.0], Modifiers::NONE);
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 60.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            // Fonts take effect on the next frame: set them up and draw nothing this one.
            if !ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new("fonts")).unwrap_or(false)) {
                PhotocraftApp::setup_context(ui.ctx(), Default::default());
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("fonts"), true));
                return;
            }
            ui.horizontal(|ui| {
                options_bar(app, ui, Tool::Roto);
            });
        },
        a,
    );
    h.run_steps(3);
    h.get_by_label("Smooth").click();
    h.run_steps(3);
    let a = h.state();
    eprintln!("status {:?} err {}", a.ui.status, a.ui.status_error);
    assert!(shapes(a)[0].points[0].smooth, "button click smoothed the point");
}

#[test]
fn a_pen_shape_stays_open_until_enter_or_a_click_on_its_first_point() {
    let ctx = egui::Context::default();
    let key = |a: &mut PhotocraftApp, k: egui::Key| {
        let raw = egui::RawInput {
            events: vec![egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }],
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| assert!(keys(a, ui.ctx())));
        out.textures_delta.clear();
    };
    let pts = [[40.0, 40.0], [160.0, 40.0], [160.0, 160.0], [40.0, 160.0]];
    // Enter joins the last point to the first.
    let mut a = app();
    a.ui.roto.mode = RotoMode::Pen;
    for p in pts {
        click(&mut a, p, Modifiers::NONE);
    }
    assert!(!shapes(&a)[0].closed, "still open while drawing");
    assert!(a.ui.roto.drawing.is_some());
    key(&mut a, egui::Key::Enter);
    assert!(shapes(&a)[0].closed && a.ui.roto.drawing.is_none());
    let area: f64 = vector::roto::roto_values(&mask(&a), photocraft_geom::Rect::new(0, 0, 300, 300)).iter().map(|v| f64::from(*v)).sum();
    assert!(area > 100.0 * 100.0, "a closed 120x120 square fills: {area}");
    // Clicking the first point closes it.
    let mut a = app();
    a.ui.roto.mode = RotoMode::Pen;
    for p in pts {
        click(&mut a, p, Modifiers::NONE);
    }
    click(&mut a, pts[0], Modifiers::NONE);
    assert!(shapes(&a)[0].closed && a.ui.roto.drawing.is_none());
    assert_eq!(shapes(&a)[0].points.len(), 4);
    // Escape finishes it open; Enter on fewer than three points leaves it open too.
    let mut a = app();
    a.ui.roto.mode = RotoMode::Pen;
    click(&mut a, pts[0], Modifiers::NONE);
    click(&mut a, pts[1], Modifiers::NONE);
    key(&mut a, egui::Key::Enter);
    assert!(!shapes(&a)[0].closed && a.ui.roto.drawing.is_none());
}

#[test]
fn the_selection_command_uses_the_panel_node_or_all_splines_and_each_spline_option() {
    let ctx = egui::Context::default();
    let mut a = with_rect();
    a.ui.roto.mode = RotoMode::Rectangle;
    drag(&mut a, [150.0, 20.0], [200.0, 80.0], Modifiers::NONE);
    let area = |a: &PhotocraftApp| -> f64 {
        let d = a.session.active().unwrap();
        photocraft_algo::selection::mask_from_surface(d.doc.selection.as_ref(), d.doc.bounds()).iter().map(|v| f64::from(*v)).sum()
    };
    assert!(crate::menus::is_enabled(&a, "roto.selectionFromMask"));
    crate::menus::invoke(&mut a, &ctx, "roto.selectionFromMask", json!({})).unwrap();
    assert_eq!(area(&a), 80.0 * 60.0 + 50.0 * 60.0, "all splines together");
    a.ui.roto.nodes = vec![shapes(&a)[1].id];
    crate::menus::invoke(&mut a, &ctx, "roto.selectionFromMask", json!({})).unwrap();
    assert_eq!(area(&a), 50.0 * 60.0, "just the node chosen in the panel");
    a.ui.roto.selection_each = true;
    a.ui.roto.nodes.clear();
    crate::menus::invoke(&mut a, &ctx, "roto.selectionFromMask", json!({})).unwrap();
    assert_eq!(area(&a), 80.0 * 60.0 + 50.0 * 60.0);
    let mut empty = app();
    assert!(!crate::menus::is_enabled(&empty, "roto.selectionFromMask"));
    assert!(crate::menus::invoke(&mut empty, &ctx, "roto.selectionFromMask", json!({})).is_err());
}

#[test]
fn the_right_click_action_adds_roto_shapes_following_the_selection() {
    let mut a = app();
    a.run("select.rect", json!({"x": 50, "y": 60, "width": 80, "height": 40})).unwrap();
    selection_to_shapes(&mut a);
    assert!(!a.ui.status_error, "{}", a.ui.status);
    let sh = shapes(&a);
    assert_eq!(sh.len(), 1);
    let xs: Vec<f64> = sh[0].points.iter().map(|p| p.pos.x).collect();
    assert!(xs.iter().cloned().fold(f64::MAX, f64::min) >= 49.0 && xs.iter().cloned().fold(f64::MIN, f64::max) <= 131.0);
    // Without a selection it says why instead of doing anything.
    let mut b = app();
    selection_to_shapes(&mut b);
    assert!(b.ui.status_error);
}
