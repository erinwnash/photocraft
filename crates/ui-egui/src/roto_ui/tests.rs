use photocraft_doc::roto::Node;
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
