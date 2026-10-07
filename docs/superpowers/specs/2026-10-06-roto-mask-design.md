# Roto mask: design

Date: 2026-10-06. Status: approved (v1).

## 1. Goal

A Nuke-Roto-style editor that generates a layer's alpha mask from bezier splines:
a tree of splines with full point editing and grouping, per-point and global soft
edges (feather), per-spline opacity `0..=1`, per-shape blend ops, group transforms,
shape tools, blur and falloff control, and round-trip import/export with Nuke.

"1:1 with Nuke" means matching Roto's **behaviour and UX**. It is a clean-room
implementation: no Foundry code or assets, and Nuke serialization is learnt from
sample files.

### Decisions already made

| Topic | Decision |
|---|---|
| Time | **Stills only.** No keyframes in v1. Stable node/point IDs keep keyframing addable later. |
| Storage | New `RotoMask` field on the layer, beside `VectorMask` (which is untouched). Native in `.pcraft`. PSD export bakes it to a raster layer mask and stores the splines in a private PSD resource for re-open. |
| Rendering | CPU evaluator is the reference/oracle. GPU evaluator is an optional backend, parity-tested, falling back to CPU. |
| Shapes | Bezier only. No B-splines. |
| Falloff | Preset list only (Linear, Smooth, Ease-in, Ease-out). No custom curves in v1. |
| v1 extras | Per-shape blend ops, group transforms, shape tools (pen, rectangle, ellipse, freehand), blur and falloff. |
| Overlap math | Switchable per roto instance (`Max` default, `Sum`, `Over`). |
| Nuke exchange | `.nk` script text, both directions. Export can also go to the clipboard; import can also come from the clipboard. |

## 2. Data model (`crates/doc/src/roto.rs`, L1, pure data)

```text
RotoMask { enabled, linked, density 0..=1, invert,
           overlap: Max|Sum|Over, backend: Auto|Cpu|Gpu, root: Group }

Node = Group | Shape        (every node has a stable NodeId)
Group { id, name, visible, locked, opacity, blend_op, transform, children[] }
Shape { id, name, visible, locked, opacity 0..=1, blend_op, invert, closed,
        points[], blur, falloff, transform }
Point { id, pos, tangent_in, tangent_out,   // handles relative to pos
        feather_pos,                        // offset of the feather handle
        feather_in, feather_out,            // feather-outline tangents
        smooth: bool }
BlendOp = Union | Subtract | Intersect | Max | Min | Multiply | Screen | Difference
Falloff = Linear | Smooth | EaseIn | EaseOut
```

- Children composite bottom to top. Each node applies its opacity and blend op
  against the accumulated alpha beneath it.
- A group renders its children to an isolated alpha, then composites it as a unit.
  Its transform and opacity apply to the subtree. Nested groups are allowed.
- `density` is the final `0..=1` multiplier, matching `LayerMask`.
- Same serde derives as other `doc` types.

## 3. Evaluator (`crates/roto`, new L2 crate)

Depends on `doc`, `geom`, `raster`, `vector`. Registered in `xtask/src/layers.rs`.

Per shape:
1. Flatten the shape outline and the feather outline into polylines at the
   document tolerance, sampled at matched parameters.
2. Fill the interior with `vector::Rasterizer` (anti-aliased, nonzero).
3. Rasterize the feather band as quads between paired samples. Alpha ramps 1 to 0
   from the shape edge to the feather edge through the falloff LUT. An inward
   feather handle ramps inward, clipping the band to the interior and reducing the
   solid fill. Fold-overs and overlaps are combined with the instance's `overlap` mode.
4. If `blur > 0`, apply a bounded separable Gaussian.
5. Invert if set, then multiply by `opacity`.

Tree: walk bottom to top, combine each node's coverage into the accumulator via its
`BlendOp`, multiply by `density`, invert if `RotoMask.invert`. Output is an `f32`
coverage array for a requested `Rect`, the same shape as `vector_mask_values`;
`compose::masks` gains one match arm.

Caching: region-of-interest tiles, plus a per-node cache keyed by a content hash, so
moving one point re-renders only its shape and the nodes above it.

### GPU backend

Same stages as wgpu compute passes (band raster, Gaussian blur, blend combine) in
`crates/gpu`. `backend: Auto` uses the GPU when an adapter exists and the document is
large enough to benefit, else the CPU. A parity corpus renders on both backends and
asserts max per-pixel difference under a small tolerance (about 1/255). Any GPU error
falls back to the CPU.

### Never-crash

Caps on point count, tree depth, blur radius and tile allocation. NaN/inf handle
positions are rejected or clamped. No `unwrap`/`expect`/panics; errors propagate.

## 4. Commands (`crates/engine/src/roto_cmds.rs`, L5)

Every action is a command (undo, control channel, MCP). IDs are validated; a stale ID
is an error, never a panic.

- **Tree:** create, delete, rename, reorder, group, ungroup, duplicate, visibility, lock.
- **Points:** add, delete, move, smooth/cusp, set tangent, set feather handle.
- **Feather:** `roto.feather.set_point`, `roto.feather.scale_selected` (pull out all
  selected points at once), `roto.feather.set_all`.
- **Shape properties:** opacity, blend op, invert, blur, falloff.
- **Instance settings:** density, invert, overlap mode, backend.
- **Transforms:** translate, rotate, scale, skew with a pivot, on the selection or a group.
- **Shape creation:** pen, rectangle, ellipse, freehand (fitted to bezier with a tolerance).
- **Output:** bake to a raster mask.
- **Nuke exchange:** `roto.export_nuke` (returns `.nk` text), `roto.import_nuke`
  (takes `.nk` text and a frame number). See section 6.

## 5. UI (`crates/ui-egui`, L6, thin)

- **Roto tool:** canvas overlay with points, handles, feather handles and the feather
  outline; marquee and shift select, arrow-key nudge, transform handles.
- **Tree panel:** shapes and groups, drag to reorder or group, eye and lock toggles,
  per-row opacity slider and blend-op dropdown.
- **Properties panel:** feather, blur and falloff for the selection, "feather all
  points" slider, instance density, overlap and backend controls.
- **Mask view:** red tint or grayscale overlay.
- **Nuke:** Export to file, Copy as Nuke nodes (clipboard), Import from file, Paste
  from Nuke (clipboard). The clipboard is touched only in this layer; the engine
  commands pass text in and out.

## 6. Nuke import/export (`roto::nuke`, isolated module)

- **Export:** writes a `.nk` fragment containing a `Roto` node whose `curves` knob holds
  the shapes. Every value is a single static key. Output is also what the clipboard gets,
  so it pastes directly into Nuke's node graph.
- **Import:** reads `Roto` and `RotoPaint` nodes from `.nk` text. Animated values are
  sampled at the requested frame (default: the frame in the pasted node, or 1) and the
  other keys are dropped. Unsupported constructs (strokes, brush shapes, tracking links,
  expressions) are skipped and reported in a warnings list rather than failing the import.
- **Mapping:** points, tangents, feather handles, shape opacity, group hierarchy, blend
  ops, invert and blur map directly. Anything with no PhotoCraft equivalent is reported.
- **Provenance:** the `curves` serialization is not officially documented. Before writing
  the parser, search for an existing permissively licensed parser or exporter (per the
  FOSS-first rule) and record provenance in `THIRD_PARTY_NOTICES.md`. Otherwise derive the
  format from sample `.nk` files you supply.
- **Robustness:** untrusted text. Bounded input size, bounded nesting, no panics, and
  fuzz-tested.

### 6.1 Format findings from `samples/nuke-roto-bezier-1.nk`

Decoded from the first sample (a 6-point closed bezier, 2048x1556 format):
- Floats are IEEE-754 single precision written as `x` + 8 hex digits (`x44800000` = 1024.0). A bare `0` is zero.
- Node text is brace-nested; `{layer Root ...}` is the root group and `{curvegroup <name> <flags> bezier ...}` is a shape.
- A shape holds two `cc` curves: the first is the shape outline, the second is the feather outline.
- Within `px 1`, each point is three `{x y}` entries: `[tangent_in, position, tangent_out]`, tangents relative to the position.
- In the feather curve, the position slot is the feather handle as an **offset from the shape point**, and the tangent slots are the feather tangents (a zero offset means no feather at that point).
- Coordinates are y-up, in format space. `{t ...}` on the root layer matches the format centre.
- Unresolved: what `{tx 1 ...}` means (it decodes to about (980.3, 973.7), near the shape centroid, so it is probably the shape's transform pivot/translate, but the points are not offset by it), and what `osw`/`osf`/`str`/`tt` encode (`osw` = 10.0 may be overall softness). The importer treats unknown attributes as warnings, and further samples (translated, rotated, opacity, blend, groups, ellipse/rectangle, inverted) will pin these down.

## 7. PSD / file format

- `.pcraft`: full `RotoMask` serialized natively.
- PSD export: baked raster layer mask for other apps, with the editable splines in a
  private PSD resource restored by PhotoCraft on re-open. If the resource is missing or
  fails to decode, the baked mask loads as an ordinary mask.

## 8. Testing

- Unit tests for the model and the evaluator.
- Golden-image tests of the CPU output against stored reference alpha.
- CPU/GPU parity corpus.
- Command tests including undo.
- Nuke round-trip: export then import yields the same shapes (within float tolerance);
  import of sample `.nk` files produces the expected shapes and warnings.
- Fuzzing: NaN, huge and degenerate geometry in the evaluator, and malformed `.nk` text
  in the importer.

## 9. Delivery in phases (one plan, ordered)

1. Data model, `.pcraft` serialization, CPU evaluator for hard-edge bezier, compositor hook.
2. Feather band, falloff, blur, overlap modes, per-shape opacity and blend ops.
3. Engine commands and history.
4. UI: roto tool, tree panel, properties panel.
5. Shape tools and group transforms.
6. Nuke import/export, including the clipboard.
7. PSD bake plus private resource.
8. GPU backend and parity tests.

## 10. Open items

- Nuke `curves` format: needs sample `.nk` files, or an existing parser to adopt.
- Whether any overlap mode should be tuned to Nuke reference renders (needs reference
  renders from you).
- Exact tolerance for CPU/GPU parity.
