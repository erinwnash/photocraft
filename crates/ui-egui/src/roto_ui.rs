//! Roto tool: draw and edit bezier splines that make a layer's alpha mask, with the Roto panel
//! (layer tree, properties, feather, Nuke exchange). The shell is thin: hit testing and gesture
//! maths live in `roto_edit.rs`, and every edit goes through the engine's `roto.*` commands.
//!
//! Tool: Select mode edits (click a point, ⇧-click toggles, drag moves the selection, drag a
//! handle to bend or feather, ⌘-drag a point pulls its soft edge out, ⌘⌥-click on the outline adds
//! a point, drag on empty canvas marquee-selects); Pen, Rectangle, Ellipse and Freehand modes
//! create shapes. Keys: Delete removes the selected points, arrows nudge (⇧ ×10), ↩ and Esc finish
//! the shape being drawn.

use egui::{Color32, Modifiers, Pos2, Rect, Stroke, vec2};
use photocraft_doc::LayerId;
use photocraft_doc::RotoMask;
use photocraft_doc::roto::{Group, Node, NodeId, PointId};
use photocraft_vector as vector;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::roto_edit::{self as edit, DragStart, Hit, P2, Selection};
use crate::state::Tool;
use crate::theme::Tokens;

/// What a drag on the canvas does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RotoMode {
    /// Select and edit points and handles.
    #[default]
    Select,
    /// Click to add points (drag for smooth handles).
    Pen,
    Rectangle,
    Ellipse,
    /// Drag a free stroke; it is fitted to bezier points on release.
    Freehand,
}

impl RotoMode {
    pub const ALL: [RotoMode; 5] = [RotoMode::Select, RotoMode::Pen, RotoMode::Rectangle, RotoMode::Ellipse, RotoMode::Freehand];

    pub fn name(self) -> &'static str {
        match self {
            RotoMode::Select => "select",
            RotoMode::Pen => "pen",
            RotoMode::Rectangle => "rectangle",
            RotoMode::Ellipse => "ellipse",
            RotoMode::Freehand => "freehand",
        }
    }

    pub fn from_name(s: &str) -> Option<RotoMode> {
        RotoMode::ALL.into_iter().find(|m| m.name() == s)
    }

    fn label(self) -> &'static str {
        match self {
            RotoMode::Select => tl!("Select"),
            RotoMode::Pen => tl!("Bezier"),
            RotoMode::Rectangle => tl!("Rectangle"),
            RotoMode::Ellipse => tl!("Ellipse"),
            RotoMode::Freehand => tl!("Freehand"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    TangentIn,
    TangentOut,
    Feather,
}

/// A drag in progress.
#[derive(Clone, Debug, PartialEq)]
enum Gesture {
    Points(DragStart, P2),
    Handle {
        shape: NodeId,
        point: PointId,
        which: Handle,
        affine: photocraft_geom::Affine,
        local_pos: P2,
    },
    /// Pulling smooth handles out of a point the Pen just placed.
    PenHandle {
        shape: NodeId,
        point: PointId,
        from: P2,
        affine: photocraft_geom::Affine,
        local_pos: P2,
    },
    Marquee {
        from: P2,
        to: P2,
        additive: bool,
    },
    Create {
        from: P2,
        to: P2,
        ellipse: bool,
    },
    Freehand(Vec<P2>),
}

/// The red overlay drawn over the image while a roto mask is edited, kept as a texture and rebuilt
/// only when the mask, the document or the preview size changes. Transient: never saved, and every
/// copy compares equal (it is a cache, not state).
#[derive(Clone, Default)]
pub struct Matte {
    key: u64,
    tex: Option<egui::TextureHandle>,
}

impl std::fmt::Debug for Matte {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Matte({})", if self.tex.is_some() { "texture" } else { "none" })
    }
}

impl PartialEq for Matte {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// Roto tool and panel state. Everything but the mode is transient.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RotoUi {
    pub mode: RotoMode,
    /// Hide the feather outline and handles on the canvas.
    pub hide_feather: bool,
    /// Show the mask as a red overlay while editing: the layer is then not masked, so the whole
    /// image stays visible to trace, with the hidden areas tinted. Off by default, the mask is
    /// applied to the layer as you draw.
    #[serde(default)]
    pub matte_overlay: bool,
    /// Make Selection evaluates each spline on its own and unions them (ignoring blend ops).
    #[serde(default)]
    pub selection_each: bool,
    #[serde(skip)]
    matte: Matte,
    /// The layer this app put in the editing view. The view is process-wide state, so the app only
    /// ever turns off a view it turned on.
    #[serde(skip)]
    view_layer: Option<photocraft_doc::LayerId>,
    #[serde(skip)]
    pub sel: Selection,
    /// Selected rows of the layer tree.
    #[serde(skip)]
    pub nodes: Vec<NodeId>,
    /// The shape the Pen is adding points to.
    #[serde(skip)]
    pub drawing: Option<NodeId>,
    #[serde(skip)]
    gesture: Option<Gesture>,
    #[serde(skip)]
    key: u64,
    /// Pasted Nuke script waiting to be imported, and what the last import reported.
    #[serde(skip)]
    pub nuke_text: String,
    #[serde(skip)]
    pub nuke_report: Vec<String>,
    /// The panel's "feather all" distance in px.
    #[serde(skip)]
    pub feather_px: f32,
}

// ---------------------------------------------------------------------------------------------
// Helpers

fn active(app: &PhotocraftApp) -> Option<(LayerId, &RotoMask)> {
    let st = app.session.active()?;
    let id = st.active_layer?;
    let m = st.doc.layer(id)?.roto_mask.as_ref()?;
    Some((id, m))
}

fn radius(app: &PhotocraftApp) -> f64 {
    6.0 / f64::from(app.current_zoom().max(0.01))
}

/// Runs a command, reporting a failure in the status bar. Returns the result on success.
fn run(app: &mut PhotocraftApp, cmd: &str, params: Value) -> Option<Value> {
    match app.run(cmd, params) {
        Ok(v) => Some(v),
        Err(e) => {
            app.ui.status = e;
            app.ui.status_error = true;
            None
        }
    }
}

/// Makes sure the active layer has a roto mask (creating one is one undo step of its own).
fn ensure_mask(app: &mut PhotocraftApp) -> bool {
    active(app).is_some() || run(app, "layer.roto.add", json!({})).is_some()
}

fn coalesce_key(app: &PhotocraftApp) -> String {
    format!("roto-{}", app.ui.roto.key)
}

fn shape_points(m: &RotoMask, id: NodeId) -> Vec<PointId> {
    match m.find(id) {
        Some(Node::Shape(s)) => s.points.iter().map(|p| p.id).collect(),
        _ => Vec::new(),
    }
}

fn local_pos(m: &RotoMask, shape: NodeId, point: PointId) -> Option<P2> {
    match m.find(shape) {
        Some(Node::Shape(s)) => s.points.iter().find(|p| p.id == point).map(|p| [p.pos.x, p.pos.y]),
        _ => None,
    }
}

fn to_screen(xf: &ViewXform, p: P2) -> Pos2 {
    xf.to_screen(p[0] as f32, p[1] as f32)
}

fn prune(app: &mut PhotocraftApp) {
    let mut sel = std::mem::take(&mut app.ui.roto.sel);
    match active(app) {
        Some((_, m)) => sel.prune(m),
        None => sel.clear(),
    }
    app.ui.roto.sel = sel;
}

// ---------------------------------------------------------------------------------------------
// Cusp, uncusp and smooth

/// The point commands: shell command id (menus and hotkeys: assign keys in Edit > Keyboard
/// Shortcuts) and the engine command it runs.
const POINT_OPS: [(&str, &str); 3] =
    [("roto.cuspPoints", "roto.point.cusp"), ("roto.uncuspPoints", "roto.point.uncusp"), ("roto.smoothPoints", "roto.point.smooth")];

/// The button label and tooltip of a point command.
fn point_texts(id: &str) -> (&'static str, &'static str) {
    match id {
        "roto.cuspPoints" => (tl!("Cusp"), tl!("Cusp points: handles move independently")),
        "roto.uncuspPoints" => (tl!("Uncusp"), tl!("Uncusp points: link the handles in a straight line")),
        _ => (tl!("Smooth"), tl!("Smooth points: build handles from the neighbours")),
    }
}

/// Is `id` one of the point commands?
pub fn handles(id: &str) -> bool {
    id == SELECTION_CMD || POINT_OPS.iter().any(|o| o.0 == id)
}

/// Shell command id of Layer > Roto Mask > Selection from Roto Mask.
const SELECTION_CMD: &str = "roto.selectionFromMask";

/// Makes the document selection from the node chosen in the Roto panel (a group or a shape), or
/// from all the splines together when none is chosen. `mode` is a selection mode.
fn make_selection(app: &mut PhotocraftApp, mode: &str) -> Result<Value, String> {
    let node = active(app).and_then(|(_, m)| app.ui.roto.nodes.first().copied().filter(|id| m.find(*id).is_some())).map_or(0, |id| id.0);
    app.run("roto.selection.make", json!({"node": node, "each": app.ui.roto.selection_each, "mode": mode}))
}

/// What the point commands act on: the selected points, else every point of a shape: the one the
/// selection or the Roto panel names, the one being drawn, or the newest shape. `None` when the
/// mask has no shape.
fn point_targets(app: &PhotocraftApp) -> Option<(NodeId, Option<Vec<PointId>>)> {
    let (_, m) = active(app)?;
    let sel = &app.ui.roto.sel;
    if let Some(shape) = sel.shape.filter(|_| !sel.points.is_empty()) {
        return Some((shape, Some(sel.points.clone())));
    }
    let is_shape = |id: &NodeId| matches!(m.find(*id), Some(Node::Shape(_)));
    let newest = m.root.children.iter().rev().find_map(|n| if let Node::Shape(s) = n { Some(s.id) } else { None });
    let shape = sel.shape.filter(is_shape).or(app.ui.roto.nodes.first().copied().filter(is_shape)).or(app.ui.roto.drawing.filter(is_shape)).or(newest)?;
    Some((shape, None))
}

/// Whether a point command can run now (`None` for other commands).
pub fn is_enabled(app: &PhotocraftApp, id: &str) -> Option<bool> {
    if id == SELECTION_CMD {
        return Some(active(app).is_some_and(|(_, m)| !m.root.children.is_empty()));
    }
    handles(id).then(|| point_targets(app).is_some())
}

/// Runs the engine command behind a point command on the current selection.
fn point_op(app: &mut PhotocraftApp, engine_cmd: &str) -> Result<Value, String> {
    let (shape, ids) = point_targets(app).ok_or_else(|| tl!("Select points (or a shape in the Roto panel) first").to_string())?;
    let mut params = json!({"shape": shape.0});
    if let Some(ids) = ids {
        params["ids"] = json!(ids.iter().map(|p| p.0).collect::<Vec<_>>());
    }
    app.run(engine_cmd, params)
}

/// Menu and hotkey entry for the point commands (`None` for other commands).
pub fn menu(app: &mut PhotocraftApp, id: &str, _params: &Value) -> Option<Result<Value, String>> {
    if id == SELECTION_CMD {
        return Some(make_selection(app, "replace"));
    }
    let (_, cmd) = POINT_OPS.iter().find(|o| o.0 == id)?;
    Some(point_op(app, cmd))
}

// ---------------------------------------------------------------------------------------------
// The editing view

/// Longest side of the matte preview, idle and while a gesture is dragging (a reduced mask is
/// evaluated, so dragging stays responsive on large documents).
const MATTE_IDLE_SIDE: u32 = 1024;
const MATTE_DRAG_SIDE: u32 = 512;

/// `mask` scaled by `s` so that evaluating it over a rect `s` times smaller gives the same
/// picture: the mask's own transform scales, and blur radii (lengths, not coordinates) scale too.
fn scaled_for_preview(mask: &RotoMask, s: f64) -> RotoMask {
    fn blurs(g: &mut Group, s: f32) {
        for n in &mut g.children {
            match n {
                Node::Shape(sh) => sh.blur *= s,
                Node::Group(c) => blurs(c, s),
            }
        }
    }
    let mut m = mask.clone();
    let t = &mut m.root.transform;
    // s * T(p) = pivot + (s * translate + (s - 1) * pivot) + R K (s * S) (p - pivot)
    t.translate = photocraft_doc::roto::V2::new(s * t.translate.x + (s - 1.0) * t.pivot.x, s * t.translate.y + (s - 1.0) * t.pivot.y);
    t.scale = photocraft_doc::roto::V2::new(t.scale.x * s, t.scale.y * s);
    blurs(&mut m.root, s as f32);
    m
}

/// The rubylith for `mask` over a `w` by `h` document: half-strength red where the mask hides the
/// image, clear where it reveals it (the usual mask overlay), at most `max_side` px on the longest
/// side. Returns the image size and its premultiplied pixels.
pub(crate) fn matte_image(mask: &RotoMask, w: u32, h: u32, max_side: u32) -> ([usize; 2], Vec<Color32>) {
    let scale = (f64::from(max_side) / f64::from(w.max(h).max(1))).min(1.0);
    let (pw, ph) = (((f64::from(w) * scale).ceil() as i32).max(1), ((f64::from(h) * scale).ceil() as i32).max(1));
    let preview = scaled_for_preview(mask, scale);
    let values = vector::roto::roto_values_auto(&preview, photocraft_geom::Rect::new(0, 0, pw, ph));
    let px = values
        .iter()
        .map(|c| {
            let a = 0.5 * (1.0 - c.clamp(0.0, 1.0));
            Color32::from_rgba_premultiplied((255.0 * a).round() as u8, 0, 0, (255.0 * a).round() as u8)
        })
        .collect();
    ([pw as usize, ph as usize], px)
}

/// Keeps the editing view in step with the tool: while the Roto tool is active on a layer that has
/// an enabled roto mask and the user has switched the mask overlay on, that mask is not applied to
/// the layer, so the whole image stays visible, and it is drawn as a red overlay instead. Called
/// every frame, before the canvas draws.
pub fn sync_view(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let wanted = (app.ui.tool == Tool::Roto && app.ui.roto.matte_overlay).then(|| active(app).filter(|(_, m)| m.enabled).map(|(id, _)| id)).flatten();
    let owned = app.ui.roto.view_layer;
    if wanted != owned {
        // End the view this app started (the layer may be gone or in another document: then the
        // command fails and the flag is cleared directly), then start the new one.
        if let Some(old) = owned {
            if app.run("view.rotoEdit", json!({"layer": old.0, "on": false})).is_err() && vector::roto::is_editing(old) {
                vector::roto::set_editing_layer(None);
            }
            app.ui.roto.view_layer = None;
        }
        if let Some(layer) = wanted
            && app.run("view.rotoEdit", json!({"layer": layer.0, "on": true})).is_ok()
        {
            app.ui.roto.view_layer = Some(layer);
        }
    }
    let Some((layer, mask)) = active(app).filter(|(id, _)| app.ui.roto.view_layer == Some(*id) && vector::roto::is_editing(*id)) else {
        app.ui.roto.matte = Matte::default();
        return;
    };
    let Some(size) = app.session.active().map(|st| st.doc.size) else { return };
    let max_side = if app.ui.roto.gesture.is_some() { MATTE_DRAG_SIDE } else { MATTE_IDLE_SIDE };
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (mask.fingerprint(), layer.0, size.width, size.height, max_side).hash(&mut h);
        h.finish()
    };
    if app.ui.roto.matte.key != key || app.ui.roto.matte.tex.is_none() {
        let (dims, px) = matte_image(mask, size.width, size.height, max_side);
        let image = egui::ColorImage { size: dims, source_size: egui::vec2(dims[0] as f32, dims[1] as f32), pixels: px };
        let tex = ctx.load_texture("roto-matte", image, egui::TextureOptions::LINEAR);
        app.ui.roto.matte = Matte { key, tex: Some(tex) };
    }
}

// ---------------------------------------------------------------------------------------------
// Canvas events

pub fn down(app: &mut PhotocraftApp, x: f64, y: f64, mods: Modifiers) {
    let p = [x, y];
    app.ui.roto.key += 1;
    match app.ui.roto.mode {
        RotoMode::Select => select_down(app, p, mods),
        RotoMode::Pen => pen_down(app, p),
        RotoMode::Rectangle => app.ui.roto.gesture = Some(Gesture::Create { from: p, to: p, ellipse: false }),
        RotoMode::Ellipse => app.ui.roto.gesture = Some(Gesture::Create { from: p, to: p, ellipse: true }),
        RotoMode::Freehand => app.ui.roto.gesture = Some(Gesture::Freehand(vec![p])),
    }
}

enum Plan {
    Nothing,
    Start(Gesture),
    Select(Selection),
    SelectAndStart(Selection, Gesture),
    Insert { shape: NodeId, index: usize, local: P2 },
}

fn select_down(app: &mut PhotocraftApp, p: P2, mods: Modifiers) {
    let tol = radius(app);
    let plan = {
        let Some((_, m)) = active(app) else {
            // No roto mask yet: a drag on empty canvas starts nothing.
            return;
        };
        let views = edit::shape_views(m);
        let sel = &app.ui.roto.sel;
        match edit::hit_test(&views, sel, p, tol) {
            Hit::Point { shape, point } if mods.command => {
                let mut s = Selection::default();
                s.click(shape, point, false);
                match (edit::view_of_shape(m, shape), local_pos(m, shape, point)) {
                    (Some(v), Some(lp)) => Plan::SelectAndStart(s, Gesture::Handle { shape, point, which: Handle::Feather, affine: v.affine, local_pos: lp }),
                    _ => Plan::Select(s),
                }
            }
            Hit::Point { shape, point } => {
                let mut s = sel.clone();
                if !(sel.contains(shape, point) && !mods.shift) {
                    s.click(shape, point, mods.shift);
                }
                match DragStart::capture(m, &s, shape, None) {
                    Some(start) if s.contains(shape, point) => Plan::SelectAndStart(s, Gesture::Points(start, p)),
                    _ => Plan::Select(s),
                }
            }
            Hit::TangentIn { shape, point } | Hit::TangentOut { shape, point } | Hit::Feather { shape, point } => {
                let which = match edit::hit_test(&views, sel, p, tol) {
                    Hit::TangentIn { .. } => Handle::TangentIn,
                    Hit::TangentOut { .. } => Handle::TangentOut,
                    _ => Handle::Feather,
                };
                match (edit::view_of_shape(m, shape), local_pos(m, shape, point)) {
                    (Some(v), Some(lp)) => Plan::Start(Gesture::Handle { shape, point, which, affine: v.affine, local_pos: lp }),
                    _ => Plan::Nothing,
                }
            }
            Hit::Segment { shape, index, pos } => {
                if mods.command && mods.alt {
                    match edit::view_of_shape(m, shape).and_then(|v| edit::to_local(&v.affine, pos)) {
                        Some(local) => Plan::Insert { shape, index: index + 1, local },
                        None => Plan::Nothing,
                    }
                } else {
                    let mut s = Selection::default();
                    s.set(shape, shape_points(m, shape), false);
                    Plan::Select(s)
                }
            }
            Hit::None => {
                let mut s = if mods.shift { sel.clone() } else { Selection::default() };
                if !mods.shift {
                    s.clear();
                }
                Plan::SelectAndStart(s, Gesture::Marquee { from: p, to: p, additive: mods.shift })
            }
        }
    };
    match plan {
        Plan::Nothing => {}
        Plan::Start(g) => app.ui.roto.gesture = Some(g),
        Plan::Select(s) => app.ui.roto.sel = s,
        Plan::SelectAndStart(s, g) => {
            app.ui.roto.sel = s;
            app.ui.roto.gesture = Some(g);
        }
        Plan::Insert { shape, index, local } => {
            if let Some(r) = run(app, "roto.point.add", json!({"shape": shape.0, "index": index, "pos": local, "smooth": false}))
                && let Some(id) = r.get("id").and_then(Value::as_u64)
            {
                app.ui.roto.sel.click(shape, PointId(id), false);
            }
        }
    }
}

fn pen_down(app: &mut PhotocraftApp, p: P2) {
    if !ensure_mask(app) {
        return;
    }
    let tol = radius(app);
    // Clicking the first point of the shape being drawn closes it and finishes.
    if let (Some(id), Some((_, m))) = (app.ui.roto.drawing, active(app)) {
        let closes =
            edit::view_of_shape(m, id).is_some_and(|v| v.points.len() >= 3 && v.points.first().is_some_and(|f| (f.pos[0] - p[0]).hypot(f.pos[1] - p[1]) < tol));
        if closes {
            let all = shape_points(m, id);
            app.ui.roto.drawing = None;
            app.ui.roto.sel.set(id, all, false);
            run(app, "roto.node.set", json!({"id": id.0, "closed": true}));
            return;
        }
    }
    let local = match active(app).map(|(_, m)| edit::root_affine(m)).and_then(|a| edit::to_local(&a, p)) {
        Some(l) => l,
        None => return,
    };
    let (shape, point) = match app.ui.roto.drawing {
        Some(id) => {
            let Some(r) = run(app, "roto.point.add", json!({"shape": id.0, "pos": local})) else { return };
            (id, r.get("id").and_then(Value::as_u64))
        }
        None => {
            let Some(r) = run(app, "roto.node.add_shape", json!({"points": [local], "closed": false})) else { return };
            let id = r.get("id").and_then(Value::as_u64).map(NodeId);
            let first = r.get("points").and_then(|v| v.get(0)).and_then(Value::as_u64);
            match id {
                Some(id) => {
                    app.ui.roto.drawing = Some(id);
                    (id, first)
                }
                None => return,
            }
        }
    };
    let Some(point) = point.map(PointId) else { return };
    app.ui.roto.sel.click(shape, point, false);
    if let Some((_, m)) = active(app)
        && let (Some(v), Some(lp)) = (edit::view_of_shape(m, shape), local_pos(m, shape, point))
    {
        app.ui.roto.gesture = Some(Gesture::PenHandle { shape, point, from: p, affine: v.affine, local_pos: lp });
    }
}

pub fn moved(app: &mut PhotocraftApp, x: f64, y: f64) {
    let p = [x, y];
    let tol = radius(app);
    let key = coalesce_key(app);
    let Some(g) = app.ui.roto.gesture.clone() else { return };
    match g {
        Gesture::Points(start, from) => {
            let delta = [x - from[0], y - from[1]];
            if (delta[0] != 0.0 || delta[1] != 0.0)
                && let Some(params) = start.move_params(delta, &key)
            {
                run(app, "roto.point.move", params);
            }
        }
        Gesture::Handle { shape, point, which, affine, local_pos } => {
            let params = match which {
                Handle::TangentIn => edit::tangent_params(shape, point, false, p, &affine, local_pos, &key),
                Handle::TangentOut => edit::tangent_params(shape, point, true, p, &affine, local_pos, &key),
                Handle::Feather => edit::feather_params(shape, point, p, &affine, local_pos, &key),
            };
            if let Some(params) = params {
                run(app, if which == Handle::Feather { "roto.feather.set_point" } else { "roto.point.set" }, params);
            }
        }
        Gesture::PenHandle { shape, point, from, affine, local_pos } => {
            if (p[0] - from[0]).hypot(p[1] - from[1]) > tol
                && let Some(mut params) = edit::tangent_params(shape, point, true, p, &affine, local_pos, &key)
            {
                params["smooth"] = json!(true);
                run(app, "roto.point.set", params);
            }
        }
        Gesture::Marquee { from, additive, .. } => app.ui.roto.gesture = Some(Gesture::Marquee { from, to: p, additive }),
        Gesture::Create { from, ellipse, .. } => app.ui.roto.gesture = Some(Gesture::Create { from, to: p, ellipse }),
        Gesture::Freehand(mut pts) => {
            if pts.last().is_none_or(|l| (l[0] - x).hypot(l[1] - y) >= tol / 3.0) {
                pts.push(p);
            }
            app.ui.roto.gesture = Some(Gesture::Freehand(pts));
        }
    }
}

pub fn up(app: &mut PhotocraftApp, x: f64, y: f64) {
    let tol = radius(app);
    let Some(g) = app.ui.roto.gesture.take() else { return };
    match g {
        Gesture::Marquee { from, additive, .. } => {
            let to = [x, y];
            if (to[0] - from[0]).abs() + (to[1] - from[1]).abs() < tol {
                return;
            }
            let picked = active(app).and_then(|(_, m)| {
                let views = edit::shape_views(m);
                edit::marquee_target(&views, from, to).map(|id| (id, edit::points_in_rect(&views, id, from, to)))
            });
            if let Some((id, pts)) = picked {
                app.ui.roto.sel.set(id, pts, additive);
            }
        }
        Gesture::Create { from, ellipse, .. } => {
            let to = [x, y];
            if (to[0] - from[0]).abs() < 2.0 || (to[1] - from[1]).abs() < 2.0 || !ensure_mask(app) {
                return;
            }
            let Some(a) = active(app).map(|(_, m)| edit::root_affine(m)) else { return };
            let (Some(l0), Some(l1)) = (edit::to_local(&a, from), edit::to_local(&a, to)) else { return };
            let rect = [l0[0].min(l1[0]), l0[1].min(l1[1]), (l0[0] - l1[0]).abs(), (l0[1] - l1[1]).abs()];
            let cmd = if ellipse { "roto.node.add_ellipse" } else { "roto.node.add_rect" };
            if let Some(r) = run(app, cmd, json!({"rect": rect})) {
                select_created(app, &r);
            }
        }
        Gesture::Freehand(mut pts) => {
            pts.push([x, y]);
            if pts.len() < 3 || !ensure_mask(app) {
                return;
            }
            let Some(a) = active(app).map(|(_, m)| edit::root_affine(m)) else { return };
            let local: Vec<P2> = pts.iter().filter_map(|q| edit::to_local(&a, *q)).collect();
            if let Some(r) = run(app, "roto.node.add_freehand", json!({"points": local, "tolerance": tol / 3.0})) {
                select_created(app, &r);
            }
        }
        Gesture::Points(..) | Gesture::Handle { .. } | Gesture::PenHandle { .. } => {}
    }
    prune(app);
}

fn select_created(app: &mut PhotocraftApp, result: &Value) {
    let (Some(id), Some(points)) = (result.get("id").and_then(Value::as_u64), result.get("points").and_then(Value::as_array)) else { return };
    let ids = points.iter().filter_map(Value::as_u64).map(PointId).collect();
    app.ui.roto.sel.set(NodeId(id), ids, false);
}

/// Keys while the Roto tool is active and no text field has focus. Returns true when one was used.
pub fn keys(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    if app.ui.tool != Tool::Roto {
        return false;
    }
    let pressed = |k: egui::Key, m: Modifiers| ctx.input_mut(|i| i.consume_key(m, k));
    // Enter finishes the shape and joins its last point to the first; Escape finishes it open.
    if let Some(id) = app.ui.roto.drawing {
        let enter = pressed(egui::Key::Enter, Modifiers::NONE);
        if enter || pressed(egui::Key::Escape, Modifiers::NONE) {
            // The finished shape is selected whole, so Smooth and Cusp act on all of its points.
            let all = active(app).map(|(_, m)| shape_points(m, id)).unwrap_or_default();
            app.ui.roto.drawing = None;
            if enter && all.len() >= 3 {
                run(app, "roto.node.set", json!({"id": id.0, "closed": true}));
            }
            app.ui.roto.sel.set(id, all, false);
            return true;
        }
    }
    if app.ui.roto.gesture.is_some() && pressed(egui::Key::Escape, Modifiers::NONE) {
        app.ui.roto.gesture = None;
        return true;
    }
    let Some(shape) = app.ui.roto.sel.shape else { return false };
    if pressed(egui::Key::Delete, Modifiers::NONE) || pressed(egui::Key::Backspace, Modifiers::NONE) {
        let ids: Vec<u64> = app.ui.roto.sel.points.iter().map(|p| p.0).collect();
        run(app, "roto.point.delete", json!({"shape": shape.0, "ids": ids}));
        prune(app);
        return true;
    }
    for (key, dx, dy) in
        [(egui::Key::ArrowLeft, -1.0, 0.0), (egui::Key::ArrowRight, 1.0, 0.0), (egui::Key::ArrowUp, 0.0, -1.0), (egui::Key::ArrowDown, 0.0, 1.0)]
    {
        // ⇧ first: egui matches modifiers logically, so the plain pattern would also take ⇧-arrow.
        for (mods, step) in [(Modifiers::SHIFT, 10.0), (Modifiers::NONE, 1.0)] {
            if pressed(key, mods) {
                let params = active(app).and_then(|(_, m)| edit::nudge_params(m, &app.ui.roto.sel, [dx * step, dy * step]));
                if let Some(params) = params {
                    run(app, "roto.point.move", params);
                }
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------
// Overlay

const OUTLINE: Color32 = Color32::from_rgb(0, 200, 255);
const SELECTED: Color32 = Color32::from_rgb(255, 176, 0);
const FEATHER: Color32 = Color32::from_rgba_premultiplied(200, 200, 200, 200);

/// Shapes, points, handles and the gesture in progress, over the canvas.
pub fn draw_overlay(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    if app.ui.tool != Tool::Roto {
        return;
    }
    let Some((_, m)) = active(app) else { return };
    // The mask over the image, under the outlines and handles.
    if let (Some(tex), Some(st)) = (&app.ui.roto.matte.tex, app.session.active()) {
        let rect = Rect::from_two_pos(to_screen(xf, [0.0, 0.0]), to_screen(xf, [f64::from(st.doc.size.width), f64::from(st.doc.size.height)]));
        let uv = if xf.flip {
            Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
        } else {
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
        };
        painter.image(tex.id(), rect, uv, Color32::WHITE);
    }
    let sel = &app.ui.roto.sel;
    for v in edit::shape_views(m) {
        let selected_shape = sel.shape == Some(v.id);
        let (col, w) = if !v.visible || v.locked {
            (Color32::GRAY, 1.0)
        } else if selected_shape {
            (SELECTED, 1.5)
        } else {
            (OUTLINE, 1.0)
        };
        let line: Vec<Pos2> = edit::outline_samples(&v, 16).into_iter().map(|(_, q)| to_screen(xf, q)).collect();
        if line.len() >= 2 {
            painter.add(if v.closed { egui::Shape::closed_line(line, Stroke::new(w, col)) } else { egui::Shape::line(line, Stroke::new(w, col)) });
        }
        if !app.ui.roto.hide_feather && v.points.iter().any(|p| p.feather.is_some()) {
            let mut f: Vec<Pos2> = edit::feather_polyline(&v).into_iter().map(|q| to_screen(xf, q)).collect();
            if v.closed
                && let Some(first) = f.first().copied()
            {
                f.push(first);
            }
            painter.add(egui::Shape::dashed_line(&f, Stroke::new(1.0, FEATHER), 4.0, 3.0));
        }
        for p in &v.points {
            let c = to_screen(xf, p.pos);
            let on = sel.contains(v.id, p.id);
            if on {
                for (tip, is_feather) in [(p.tangent_in, false), (p.tangent_out, false)].into_iter().chain(p.feather.map(|f| (f, true))) {
                    let t = to_screen(xf, tip);
                    painter.line_segment([c, t], Stroke::new(1.0, if is_feather { FEATHER } else { SELECTED }));
                    if is_feather {
                        painter.add(egui::Shape::convex_polygon(
                            vec![t + vec2(0.0, -4.0), t + vec2(4.0, 0.0), t + vec2(0.0, 4.0), t + vec2(-4.0, 0.0)],
                            Color32::WHITE,
                            Stroke::new(1.0, FEATHER),
                        ));
                    } else if tip != p.pos {
                        painter.circle_filled(t, 3.0, SELECTED);
                    }
                }
            } else if let Some(f) = p.feather {
                let t = to_screen(xf, f);
                painter.circle_stroke(t, 2.5, Stroke::new(1.0, FEATHER));
            }
            let r = Rect::from_center_size(c, vec2(6.0, 6.0));
            painter.rect_filled(r, 0.0, if on { SELECTED } else { Color32::WHITE });
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, if selected_shape { SELECTED } else { OUTLINE }), egui::StrokeKind::Inside);
        }
    }
    let accent = Tokens::get(painter.ctx()).accent;
    match &app.ui.roto.gesture {
        Some(Gesture::Marquee { from, to, .. }) => {
            let r = Rect::from_two_pos(to_screen(xf, *from), to_screen(xf, *to));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
        }
        Some(Gesture::Create { from, to, ellipse }) => {
            let r = Rect::from_two_pos(to_screen(xf, *from), to_screen(xf, *to));
            if *ellipse {
                painter.add(egui::epaint::EllipseShape::stroke(r.center(), r.size() / 2.0, Stroke::new(1.0, accent)));
            } else {
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
            }
        }
        Some(Gesture::Freehand(pts)) => {
            let line: Vec<Pos2> = pts.iter().map(|q| to_screen(xf, *q)).collect();
            if line.len() >= 2 {
                painter.add(egui::Shape::line(line, Stroke::new(1.0, accent)));
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Options bar

/// Options bar for the Roto tool; false for other tools.
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui, tool: Tool) -> bool {
    if tool != Tool::Roto {
        return false;
    }
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!("Mode:")).color(t.text_dim).size(12.0));
    let opts: Vec<(String, &str)> = RotoMode::ALL.iter().map(|m| (m.name().to_string(), m.label())).collect();
    let mut cur = app.ui.roto.mode.name().to_string();
    if crate::widgets::dropdown(ui, "roto-mode", &mut cur, &opts, 96.0) {
        app.ui.roto.mode = RotoMode::from_name(&cur).unwrap_or_default();
        app.ui.roto.drawing = None;
        app.ui.roto.gesture = None;
    }
    crate::widgets::vline(ui, 22.0);
    let can_edit_points = point_targets(app).is_some();
    for (id, cmd) in POINT_OPS {
        let (label, tip) = point_texts(id);
        // The tooltip names the key the user mapped to the command, if any.
        let tip = crate::shortcuts::tip_label(app, tip, id);
        let clicked = ui.add_enabled_ui(can_edit_points, |ui| crate::widgets::secondary_button(ui, label, 56.0).on_hover_text(tip).clicked()).inner;
        if clicked && let Err(e) = point_op(app, cmd) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
    crate::widgets::vline(ui, 22.0);
    let can_select = is_enabled(app, SELECTION_CMD) == Some(true);
    let tip = crate::shortcuts::tip_label(app, tl!("Make a selection from the node chosen in the Roto panel, or from all the splines"), SELECTION_CMD);
    let make = ui.add_enabled_ui(can_select, |ui| crate::widgets::secondary_button(ui, tl!("To Selection"), 96.0).on_hover_text(tip).clicked()).inner;
    crate::widgets::checkbox(ui, &mut app.ui.roto.selection_each, tl!("Each spline"));
    if make && let Err(e) = make_selection(app, "replace") {
        app.ui.status = e;
        app.ui.status_error = true;
    }
    crate::widgets::vline(ui, 22.0);
    let mut show = !app.ui.roto.hide_feather;
    if crate::widgets::checkbox(ui, &mut show, tl!("Show feather")).changed() {
        app.ui.roto.hide_feather = !show;
    }
    crate::widgets::vline(ui, 22.0);
    crate::widgets::checkbox(ui, &mut app.ui.roto.matte_overlay, tl!("Show mask overlay"));
    crate::widgets::vline(ui, 22.0);
    let hint = match app.ui.roto.mode {
        RotoMode::Select => tl!("Drag points and handles · ⌘-drag a point to feather · ⌘⌥-click the outline to add a point · Delete removes"),
        RotoMode::Pen => tl!("Click to add points, drag for smooth handles · click the first point to close · ↩ finish"),
        RotoMode::Rectangle | RotoMode::Ellipse => tl!("Drag to draw the shape"),
        RotoMode::Freehand => tl!("Drag a stroke; it is fitted to bezier points"),
    };
    ui.label(egui::RichText::new(hint).color(t.text_dim).size(12.0));
    true
}

// ---------------------------------------------------------------------------------------------
// Panel

/// Rows queued while the panel is drawn, applied afterwards (the mask is borrowed while drawing).
type Actions = Vec<(&'static str, Value)>;

fn node_name(n: &Node) -> (u64, &str) {
    match n {
        Node::Shape(s) => (s.id.0, &s.name),
        Node::Group(g) => (g.id.0, &g.name),
    }
}

fn tree_rows(app: &mut PhotocraftApp, ui: &mut egui::Ui, nodes: &[Node], depth: usize, acts: &mut Actions) {
    let blend_opts: Vec<(String, &str)> = [
        ("union", tl!("Union")),
        ("subtract", tl!("Subtract")),
        ("intersect", tl!("Intersect")),
        ("max", tl!("Max")),
        ("min", tl!("Min")),
        ("multiply", tl!("Multiply")),
        ("difference", tl!("Difference")),
    ]
    .iter()
    .map(|(k, l)| (k.to_string(), *l))
    .collect();
    // Top of the tree is drawn first: the last node composites on top.
    for n in nodes.iter().rev() {
        let (id, name) = node_name(n);
        let (mut visible, mut locked, mut opacity, blend) = match n {
            Node::Shape(s) => (s.visible, s.locked, s.opacity, s.blend_op),
            Node::Group(g) => (g.visible, g.locked, g.opacity, g.blend_op),
        };
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 12.0);
            if crate::widgets::checkbox(ui, &mut visible, "").changed() {
                acts.push(("roto.node.set", json!({"id": id, "visible": visible})));
            }
            if crate::widgets::checkbox(ui, &mut locked, "L").changed() {
                acts.push(("roto.node.set", json!({"id": id, "locked": locked})));
            }
            let on = app.ui.roto.nodes.contains(&NodeId(id));
            let label = if matches!(n, Node::Group(_)) { format!("▸ {name}") } else { name.to_string() };
            if ui.selectable_label(on, label).clicked() {
                let additive = ui.input(|i| i.modifiers.command || i.modifiers.shift);
                if !additive {
                    app.ui.roto.nodes.clear();
                }
                match app.ui.roto.nodes.iter().position(|x| x.0 == id) {
                    Some(i) if additive => {
                        app.ui.roto.nodes.remove(i);
                    }
                    _ => app.ui.roto.nodes.push(NodeId(id)),
                }
                if let Node::Shape(s) = n {
                    let pts = s.points.iter().map(|p| p.id).collect();
                    app.ui.roto.sel.set(s.id, pts, false);
                }
            }
            if crate::widgets::value_field(ui, &mut opacity, 0.0..=1.0, "", 46.0).changed() {
                acts.push(("roto.node.set", json!({"id": id, "opacity": opacity})));
            }
            let mut cur = blend_key(blend).to_string();
            if crate::widgets::dropdown(ui, &format!("roto-blend-{id}"), &mut cur, &blend_opts, 84.0) {
                acts.push(("roto.node.set", json!({"id": id, "blendOp": cur})));
            }
        });
        if let Node::Group(g) = n {
            tree_rows(app, ui, &g.children, depth + 1, acts);
        }
    }
}

fn blend_key(b: photocraft_doc::roto::BlendOp) -> &'static str {
    use photocraft_doc::roto::BlendOp as B;
    match b {
        B::Union => "union",
        B::Subtract => "subtract",
        B::Intersect => "intersect",
        B::Max => "max",
        B::Min => "min",
        B::Multiply => "multiply",
        B::Difference => "difference",
    }
}

fn find_shape(m: &RotoMask, id: NodeId) -> Option<&photocraft_doc::roto::Shape> {
    match m.find(id) {
        Some(Node::Shape(s)) => Some(s),
        _ => None,
    }
}

fn count_shapes(g: &Group) -> usize {
    g.children
        .iter()
        .map(|n| match n {
            Node::Shape(_) => 1,
            Node::Group(c) => count_shapes(c),
        })
        .sum()
}

/// The Roto panel: instance settings, layer tree, selected-shape properties, feather, Nuke.
pub fn panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(mask) = active(app).map(|(_, m)| m.clone()) else {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("The active layer has no roto mask.")).color(t.text_dim));
        if crate::widgets::secondary_button(ui, tl!("Add Roto Mask"), 120.0).clicked() {
            run(app, "layer.roto.add", json!({}));
        }
        return;
    };
    let mut acts: Actions = Vec::new();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let lbl = |ui: &mut egui::Ui, s: &str| {
            ui.label(egui::RichText::new(s).color(t.text_dim).size(12.0));
        };
        // Instance settings.
        ui.horizontal_wrapped(|ui| {
            let (mut enabled, mut linked, mut invert, mut density) = (mask.enabled, mask.linked, mask.invert, mask.density);
            if crate::widgets::checkbox(ui, &mut enabled, tl!("Enabled")).changed() {
                acts.push(("roto.instance.set", json!({"enabled": enabled})));
            }
            if crate::widgets::checkbox(ui, &mut linked, tl!("Linked")).changed() {
                acts.push(("roto.instance.set", json!({"linked": linked})));
            }
            if crate::widgets::checkbox(ui, &mut invert, tl!("Invert")).changed() {
                acts.push(("roto.instance.set", json!({"invert": invert})));
            }
            lbl(ui, tl!("Density"));
            if crate::widgets::value_field(ui, &mut density, 0.0..=1.0, "", 52.0).changed() {
                acts.push(("roto.instance.set", json!({"density": density})));
            }
        });
        ui.horizontal_wrapped(|ui| {
            lbl(ui, tl!("Overlap"));
            let overlap_opts: Vec<(String, &str)> = vec![("max".into(), tl!("Max")), ("sum".into(), tl!("Sum")), ("over".into(), tl!("Over"))];
            let mut o = match mask.overlap {
                photocraft_doc::roto::OverlapMode::Max => "max",
                photocraft_doc::roto::OverlapMode::Sum => "sum",
                photocraft_doc::roto::OverlapMode::Over => "over",
            }
            .to_string();
            if crate::widgets::dropdown(ui, "roto-overlap", &mut o, &overlap_opts, 70.0) {
                acts.push(("roto.instance.set", json!({"overlap": o})));
            }
            lbl(ui, tl!("Backend"));
            let backend_opts: Vec<(String, &str)> = vec![("auto".into(), tl!("Auto")), ("cpu".into(), "CPU"), ("gpu".into(), "GPU")];
            let mut b = match mask.backend {
                photocraft_doc::roto::Backend::Auto => "auto",
                photocraft_doc::roto::Backend::Cpu => "cpu",
                photocraft_doc::roto::Backend::Gpu => "gpu",
            }
            .to_string();
            if crate::widgets::dropdown(ui, "roto-backend", &mut b, &backend_opts, 70.0) {
                acts.push(("roto.instance.set", json!({"backend": b})));
            }
        });
        ui.separator();
        // Layer tree and its actions.
        ui.horizontal_wrapped(|ui| {
            let ids: Vec<u64> = app.ui.roto.nodes.iter().map(|n| n.0).collect();
            let one = ids.first().copied();
            if crate::widgets::secondary_button(ui, tl!("Group"), 56.0).clicked() && !ids.is_empty() {
                acts.push(("roto.node.group", json!({"ids": ids})));
            }
            if crate::widgets::secondary_button(ui, tl!("Ungroup"), 64.0).clicked()
                && let Some(id) = one
            {
                acts.push(("roto.node.ungroup", json!({"id": id})));
            }
            if crate::widgets::secondary_button(ui, tl!("Duplicate"), 70.0).clicked() && !ids.is_empty() {
                acts.push(("roto.node.duplicate", json!({"ids": ids})));
            }
            if crate::widgets::secondary_button(ui, tl!("Delete"), 56.0).clicked() && !ids.is_empty() {
                acts.push(("roto.node.delete", json!({"ids": ids})));
            }
            for (label, delta) in [("▲", 1i64), ("▼", -1)] {
                if crate::widgets::secondary_button(ui, label, 26.0).clicked()
                    && let Some(id) = one
                    && let Some((_, i)) = mask.locate(NodeId(id))
                {
                    acts.push(("roto.node.reorder", json!({"id": id, "index": (i as i64 + delta).max(0)})));
                }
            }
        });
        tree_rows(app, ui, &mask.root.children, 0, &mut acts);
        if mask.root.children.is_empty() {
            lbl(ui, tl!("Draw a shape with the Roto tool, or import one from Nuke."));
        }
        ui.separator();
        // Selected shape.
        let selected_shape = app.ui.roto.nodes.first().and_then(|id| find_shape(&mask, *id));
        if let Some(s) = selected_shape {
            lbl(ui, &format!("{} — {}", tl!("Shape"), s.name));
            ui.horizontal_wrapped(|ui| {
                let (mut blur, mut invert) = (s.blur, s.invert);
                lbl(ui, tl!("Blur"));
                if crate::widgets::value_field(ui, &mut blur, 0.0..=1000.0, "px", 62.0).changed() {
                    acts.push(("roto.node.set", json!({"id": s.id.0, "blur": blur})));
                }
                if crate::widgets::checkbox(ui, &mut invert, tl!("Invert")).changed() {
                    acts.push(("roto.node.set", json!({"id": s.id.0, "invert": invert})));
                }
                lbl(ui, tl!("Falloff"));
                let fall_opts: Vec<(String, &str)> = vec![
                    ("linear".into(), tl!("Linear")),
                    ("smooth".into(), tl!("Smooth")),
                    ("easeIn".into(), tl!("Ease in")),
                    ("easeOut".into(), tl!("Ease out")),
                ];
                let mut f = match s.falloff {
                    photocraft_doc::roto::Falloff::Linear => "linear",
                    photocraft_doc::roto::Falloff::Smooth => "smooth",
                    photocraft_doc::roto::Falloff::EaseIn => "easeIn",
                    photocraft_doc::roto::Falloff::EaseOut => "easeOut",
                }
                .to_string();
                if crate::widgets::dropdown(ui, "roto-falloff", &mut f, &fall_opts, 84.0) {
                    acts.push(("roto.node.set", json!({"id": s.id.0, "falloff": f})));
                }
            });
        }
        // Feather: all points at once, or the selected ones.
        lbl(ui, tl!("Feather"));
        ui.horizontal_wrapped(|ui| {
            lbl(ui, tl!("All points"));
            let mut px = app.ui.roto.feather_px;
            if crate::widgets::value_field(ui, &mut px, -500.0..=500.0, "px", 70.0).changed() {
                app.ui.roto.feather_px = px;
                let targets: Vec<u64> = app.ui.roto.nodes.iter().map(|n| n.0).collect();
                let params = if targets.is_empty() {
                    json!({"distance": px, "coalesce": "roto-feather-all"})
                } else {
                    json!({"distance": px, "ids": targets, "coalesce": "roto-feather-all"})
                };
                acts.push(("roto.feather.set_all", params));
            }
            if let Some(shape) = app.ui.roto.sel.shape.filter(|_| !app.ui.roto.sel.points.is_empty()) {
                let pts: Vec<u64> = app.ui.roto.sel.points.iter().map(|p| p.0).collect();
                for (label, factor) in [("×½", 0.5), ("×2", 2.0), (tl!("Clear"), 0.0)] {
                    if crate::widgets::secondary_button(ui, label, 46.0).clicked() {
                        acts.push(("roto.feather.scale_selected", json!({"shape": shape.0, "ids": pts, "factor": factor})));
                    }
                }
            }
        });
        ui.separator();
        // Nuke exchange and baking.
        lbl(ui, "Nuke");
        ui.horizontal_wrapped(|ui| {
            if crate::widgets::secondary_button(ui, tl!("Copy as Nuke nodes"), 140.0).clicked()
                && let Some(r) = run(app, "roto.export_nuke", json!({}))
                && let Some(text) = r.get("text").and_then(Value::as_str)
            {
                ui.ctx().copy_text(text.to_string());
                app.ui.status = format!("{} {}", count_shapes(&mask.root), tl!("shape(s) copied for Nuke"));
                app.ui.status_error = false;
            }
            if crate::widgets::secondary_button(ui, tl!("Rasterize to layer mask"), 160.0).clicked() {
                acts.push(("layer.rasterize.rotoMask", json!({})));
            }
        });
        ui.add(
            egui::TextEdit::multiline(&mut app.ui.roto.nuke_text)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text(tl!("Paste a copied Nuke Roto node here")),
        );
        ui.horizontal(|ui| {
            for (label, mode) in [(tl!("Import (replace)"), "replace"), (tl!("Import (append)"), "append")] {
                if crate::widgets::secondary_button(ui, label, 120.0).clicked() && !app.ui.roto.nuke_text.trim().is_empty() {
                    acts.push(("roto.import_nuke", json!({"text": app.ui.roto.nuke_text.clone(), "mode": mode})));
                }
            }
        });
        for w in &app.ui.roto.nuke_report {
            lbl(ui, w);
        }
    });
    for (cmd, params) in acts {
        let result = run(app, cmd, params);
        if cmd == "roto.import_nuke"
            && let Some(r) = result
        {
            app.ui.roto.nuke_report =
                r.get("warnings").and_then(Value::as_array).map(|w| w.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            app.ui.roto.nuke_text.clear();
        }
        if matches!(cmd, "roto.node.delete" | "roto.node.group" | "roto.node.ungroup" | "roto.node.duplicate" | "roto.import_nuke" | "layer.rasterize.rotoMask")
        {
            app.ui.roto.nodes.clear();
        }
    }
    prune(app);
}

#[cfg(test)]
mod tests;
