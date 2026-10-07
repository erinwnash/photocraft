# Roto for Photoshop: plugin architecture

Status: proposal (written 2026-10-07, after the roto mask feature landed on `feat/roto-mask`).
Audience: whoever builds the Photoshop plugin. Read `docs/superpowers/specs/2026-10-06-roto-mask-design.md`
first for what the roto mask is; this document is about getting it into Photoshop.

**Goal.** Photoshop users draw and edit bezier splines with per-point feather, per-shape opacity, blend ops and
groups, and the result drives a layer's alpha mask, with the same maths and the same Nuke round trip PhotoCraft
has, by running PhotoCraft's roto core inside a Photoshop plugin.

**Non-goals (v1).** A roto *tool on Photoshop's own canvas* (see §3, probably impossible), animation, GPU
evaluation inside Photoshop, Photoshop on the web, Windows on ARM.

How sure each claim is, used throughout:

| Tag | Meaning |
|---|---|
| **[Adobe]** | Stated in Adobe's own documentation (linked in §12) |
| **[Community]** | Reported on the Adobe developer forums; probably true, version dependent |
| **[Assumption]** | Our inference. A spike in §9 must confirm it before anything depends on it |

---

## 1. What we can reuse from PhotoCraft

The roto feature was built in layers on purpose. What is plugin-ready, and what is stuck to the app:

| Piece | Where | Reusable in a plugin? |
|---|---|---|
| Data model: shapes, groups, points, feather, blend ops, validation caps, tree operations, `apply_affine`, `fingerprint` | `crates/doc/src/roto.rs` (depends on `geom`, `color`, `raster` and `serde`) | Yes. Pure data. |
| Evaluator: outline AA, feather band, blur, blend, groups, `Executor` tree walk, CPU oracle | `crates/vector/src/roto/` (depends on `doc`, `geom`, `raster`, `color`) | Yes. This is the part that must match PhotoCraft exactly. |
| Nuke `.nk` import and export | `crates/io/src/nuke.rs` | Logic yes; the crate no. `photocraft-io` drags in PSD, codecs, raw, text. Needs extracting (§7). |
| Edit semantics: ~25 `roto.*` commands with validation and rollback | `crates/engine/src/roto_cmds.rs` | Semantics yes; code no. It is written against `Session` (document, history). Needs a pure core (§7). |
| Hit testing, selection, gesture to command mapping (no egui types) | `crates/ui-egui/src/roto_edit.rs` | Logic yes; location no. Move it out of the egui crate (§7). |
| GPU backend | `crates/gpu/src/roto.rs` | No. wgpu inside Photoshop's process is out of scope. The CPU oracle is the plugin's evaluator. |
| PSD bake plus `PcRM` block | `crates/io/src/roto_map.rs` | The *idea* yes (hash the baked mask, restore only if unchanged), reused in §5.4. Photoshop will not keep our private PSD block. |

Measured on this machine (3840×2160): the CPU evaluator takes ~80 ms for one hard shape and ~860 ms for one shape
with a 25 px blur. Fine for a final render; the panel must evaluate a *reduced* resolution while dragging (§5.3).

---

## 2. What Photoshop gives a plugin

### 2.1 Plugin types

| Type | Language | Status | Fit |
|---|---|---|---|
| **UXP plugin** (panels and commands) | JavaScript, HTML, CSS (a subset) | Adobe's current model **[Adobe]** | **Host for the UI.** |
| **UXP hybrid plugin** (UXP plus a native `.uxpaddon`) | JS plus C++ | Photoshop 24.2.0 or later **[Adobe]** | **Host for the Rust core.** |
| UXP plugin with **WebAssembly** | JS plus wasm | Works; crash reports on Photoshop 2025 **[Community]** | Fallback (§4, option B). |
| C++ plug-in SDK (filters `.8bf`, automation, file formats) | C and C++ | Long-lived, needs an Adobe Developer account to download the SDK **[Adobe]**. A filter plug-in gets the selection but the mask data pointer has been reported null **[Community]** | Not a fit: it filters pixels, it does not manage masks or a persistent editor. |
| CEP and ExtendScript | JS | Legacy, being replaced by UXP | Rejected. |

### 2.2 UXP facts that shape the design

* **Manifest.** `manifestVersion` 5 gives panels, commands and permissions (`network`, `localFileSystem` as
  `request`/`plugin`/`fullAccess`, `clipboard`, `webview`, `launchProcess`, `ipc`); host `{"app":"PS","minVersion":"23.3.0"}`
  **[Adobe]**. A hybrid plugin needs `manifestVersion` 6 or later, `"enableAddon": true` and
  `"addon": {"name": "<file>.uxpaddon"}` **[Adobe]**.
* **Panels** are persistent, dockable and know the user's current selection; commands are menu items **[Adobe]**.
* **Imaging API** (`require("photoshop").imaging`) **[Adobe]**:
  * `getPixels({documentID, layerID, sourceBounds, targetSize, colorSpace, componentSize, applyAlpha, …})` returns
    `{imageData, sourceBounds, level}`; omit `layerID` for the document composite. `targetSize` gives a scaled read.
  * `putPixels({layerID, imageData, replace, targetBounds, commandName})` writes a pixel layer.
  * `getLayerMask({layerID, kind: "user" | "vector", sourceBounds, targetSize})` reads a mask;
    `putLayerMask({layerID, imageData, replace, targetBounds, commandName})` writes only `kind: "user"`.
  * `getSelection` and `putSelection` read and write the selection as a grayscale plane.
  * `PhotoshopImageData` has `width`, `height`, `components`, `componentSize` (8, 16, 32), `getData()` (typed array)
    and `dispose()` (call it, the memory is native). Masks and selections use `components: 1`,
    `colorSpace: "Grayscale"`, `colorProfile: "Gray Gamma 2.2"`.
  * `createImageDataFromBuffer` builds image data from a typed array; `encodeImageData` makes a JPEG for UXP elements.
* **There is no DOM call to create or remove a layer mask**; it must be done with `batchPlay` (Action Manager)
  **[Community]**. Writing masks through the imaging API has reported problems: errors on fill layers, and a bug where
  blocks of all-zero pixels turned white **[Community]**. The workaround people use is `putSelection` plus a
  `batchPlay` "make mask from selection".
* **Document changes need `executeAsModal`** and are grouped into history states by the plugin **[Adobe]**.
* **HTML canvas** exists with a 2D context for basic shapes only; no WebGL **[Adobe]**.
* **Persistence.** The documented, working place for plugin data that survives a PSD save is **XMP**, per document
  (`XMPMetadataAsUTF8` through `batchPlay`) or per layer **[Community]**. Plain UXP storage (`plugin-data://`) is
  per machine, not per document.
* **Cursor and keyboard** limits inside panels: the CSS cursor reverts to the arrow once a drag starts, and keyboard
  shortcuts stop working after a click inside a `webview` on Windows **[Community]**.

### 2.3 Hybrid plugin facts (the native part)

All **[Adobe]** unless noted.

* A `.uxpaddon` is a dynamic library (`.dll` on Windows, `.dylib` on macOS) renamed with that extension.
* The SDK ("UXP Hybrid Plugin SDK", from the Adobe Developer Console) provides headers: `UxpAddon.h` with
  `UXP_ADDON_INIT(init)` and `UXP_ADDON_TERMINATE(terminate)`, `UxpAddonShared.h` with an API "closely mirroring
  Node-API", `UxpAddonTypes.h`. JS loads it with `require("name.uxpaddon")` and calls its exports.
* Threading: init and terminate run on the main thread; **calls from JS run on a dedicated scripting thread**; async
  work uses `uxp_addon_create_promise`, `uxp_addon_schedule_on_main_queue`, `uxp_addon_schedule_on_javascript_queue`.
* Targets: **macOS arm64, macOS x64, Windows x64**. Windows on ARM is not mentioned.
* **Creative Cloud Marketplace** requires all three architectures (a package missing one is rejected), macOS
  binaries signed *and notarized* with a valid Apple Developer ID (not self-signed, valid for at least a year), a
  50 MB bundle limit, and users must enter **OS admin credentials** to install or update a hybrid plugin.
  Independent distribution (`.ccx` shared directly) is possible with fewer architectures.
* The SDK documentation describes C++. Nothing in it forbids a Rust library behind a C++ shim; **[Assumption]**
  until spike S2 passes.

---

## 3. The constraint that decides the UX: no tool on Photoshop's canvas

I found no documented UXP API to draw on the document canvas or to register an interactive tool with handles.
Panels are the extension surface. **[Assumption: confirm with Adobe or the forums in spike S1.]**

So the roto editor cannot be a tool in Photoshop's toolbox the way it is in PhotoCraft. The editor lives **in the
panel**, with its own viewport:

```
+--------------------------------------------------+
| Roto panel                                       |
|  +--------------------------------------------+  |
|  |  viewport: the document composite          |  |
|  |  (getPixels, reduced) shown in an <img>,   |  |
|  |  with a <canvas> 2D overlay on top:        |  |
|  |  outlines, points, handles, feather        |  |
|  +--------------------------------------------+  |
|  tools: select | bezier | rect | ellipse | free   |
|  tree: shapes and groups, opacity, blend, eye     |
|  properties: blur, falloff, feather, density      |
|  Nuke: copy / import      [Apply to layer mask]   |
+--------------------------------------------------+
```

This is a real cost: pan and zoom, pointer capture and cursor feedback are re-done in the panel, and the user edits a
*picture of* the canvas. It is the same trade-off other UXP editing plugins make. If S1 finds a canvas overlay API,
only the viewport changes; nothing below it does.

---

## 4. Options

| | A. Hybrid: UXP panel + Rust core in an addon | B. UXP panel + Rust core as WebAssembly | C. C++ filter plug-in with its own dialog |
|---|---|---|---|
| Core runs as | native, in-process, fast | wasm, single-threaded, slower | native |
| Distribution | 3 architectures, signing, notarization, admin install | one `.ccx`, no native code review | per platform `.8bf`, SDK licence |
| Photoshop versions | 24.2.0 or later | UXP versions; **crashes reported on Photoshop 2025 [Community]** | very wide |
| Persistent panel and editing | yes | yes | modal dialog only |
| Risk | packaging complexity | platform stability | mask access, no persistence |
| **Verdict** | **Recommended** | **Prototype and fallback** | Rejected |

**Recommendation: A for the product, B as the fastest way to prove the editor before the native pipeline exists.**
The core is the same Rust either way (§6), so building B first costs only a second build target. The wasm build is
also the cheapest first spike: PhotoCraft already builds for `wasm32` (`cargo xtask wasm`).

---

## 5. Recommended architecture

### 5.1 Layers

```
 Photoshop
   |  UXP APIs: photoshop.app, imaging, batchPlay, executeAsModal
   v
 UXP plugin (JavaScript)                         <- plugin/ (new, in its own folder)
   panel UI, viewport, overlay drawing, pointer events,
   Photoshop I/O (read composite, write mask), XMP persistence, history
   |  require("pcroto.uxpaddon")   JSON strings and byte buffers across the boundary
   v
 Native addon (C++ shim, ~300 lines)             <- plugin/addon/
   exports pcroto_* to JS; marshals strings and ArrayBuffers; async via promises
   |  C ABI (extern "C")
   v
 photocraft-roto-ffi (Rust, cdylib + staticlib)  <- crates/roto-ffi (new)
   handle-based API, no panics across the boundary, versioned
   |
   v
 photocraft-roto-ops, -edit, -exchange (Rust)    <- extracted crates, see §7
   +- photocraft-vector (evaluator)  +- photocraft-doc (model)
```

The addon is deliberately thin: all roto knowledge is in Rust, all Photoshop knowledge is in JS, and the C++ only
moves bytes. That keeps the part that needs three signed binaries as small as possible.

### 5.2 The C ABI (proposal)

Handle-based, UTF-8 JSON for structured data, raw buffers for planes, every call returns an error code and the
error text is fetched separately. No Rust type crosses the boundary.

```c
uint32_t pcroto_abi_version(void);                         // bump on any change
int32_t  pcroto_new(const char* roto_json, void** out);    // "" = empty mask; validates (caps, finiteness)
void     pcroto_free(void* h);
int32_t  pcroto_exec(void* h, const char* cmd, const char* params_json, char** result_json);
                                                           // the roto.* commands, same names and params as the engine
int32_t  pcroto_state(void* h, char** json);               // the whole mask, for saving to XMP and for the tree UI
int32_t  pcroto_hit(void* h, const char* sel_json, double x, double y, double radius, char** json);
int32_t  pcroto_eval_u8(void* h, int32_t x0, int32_t y0, int32_t w, int32_t h, uint8_t* out);
                                                           // exactly w*h bytes, 0..255, for putLayerMask or a preview
int32_t  pcroto_import_nuke(void* h, const char* text, double doc_height, const char* mode, char** report);
int32_t  pcroto_export_nuke(void* h, double w, double h, char** text);
uint64_t pcroto_mask_hash(const uint8_t* plane, int32_t w, int32_t h);  // same hash as PSD export (§5.4)
const char* pcroto_last_error(void);                       // thread-local
void     pcroto_free_string(char* s);
```

Rules for the Rust side: `catch_unwind` at every entry point (a panic becomes an error code, never an abort inside
Photoshop); all input bounded (the model already caps points, depth, nodes, coordinates, blur, feather and the
evaluator caps pixels); no global mutable state except what the handle owns; no threads the host cannot see.

### 5.3 Rendering and responsiveness

* **Viewport**: `getPixels({targetSize})` of the document composite, one request per zoom or document change, shown
  with `encodeImageData` in an `<img>`. Not re-read while dragging.
* **While dragging**: evaluate a *reduced* mask (scale the mask's coordinates, do not blur at full radius), draw it as
  a tinted overlay in the panel. At 1/4 scale a 4K mask is ~1 MP: tens of milliseconds on the CPU oracle.
* **On release / "Apply"**: evaluate full resolution once, off the scripting thread, then write the layer mask.
* The CPU evaluator is the only evaluator in the plugin. It is exactly what PhotoCraft's exports use, so the
  mask Photoshop receives is the one PhotoCraft would produce.

### 5.4 Getting the result into Photoshop, and getting it back

**Write** (inside one `executeAsModal`, one history state named "Roto"):

1. If the layer has no user mask: create one with `batchPlay` (no DOM API). Prefer a *reveal all* mask.
2. `pcroto_eval_u8` for the layer's bounds, wrap with `createImageDataFromBuffer({components: 1,
   colorSpace: "Grayscale", colorProfile: "Gray Gamma 2.2", …})`, then `imaging.putLayerMask`.
3. If `putLayerMask` fails or mangles (fill layers, all-zero blocks, Lab; all **[Community]**-reported), fall back to
   `putSelection` plus a `batchPlay` "make mask from selection". Spike S3 decides which path is the default.
4. **An existing pixel mask** is multiplied into the result exactly as PhotoCraft's PSD export does
   (`existing × roto`), because Photoshop has one user mask per layer. Keep the pre-bake plane (compressed) in the saved
   state so the user can undo the bake later, again as PhotoCraft does (`crates/io/src/roto_map.rs`).

**Persist** the editable splines where Photoshop keeps them: a **per-layer XMP property** in a
`photocraft:` namespace, holding `{version, roto (the state JSON), bakedMaskHash, preBakeMask?}`. Photoshop discards
private PSD blocks on save, but XMP survives **[Community]**; verify in S4 across save, close, reopen and *Save As*.
On open, restore the splines only if the layer mask still hashes to `bakedMaskHash`; otherwise the user edited the
mask in Photoshop, so keep it as a plain mask and say so. This is the same rule PhotoCraft's PSD round trip uses, and
`pcroto_mask_hash` is the same function.

**PhotoCraft files**: a PSD saved by PhotoCraft has the baked mask plus a `PcRM` block; the plugin cannot read the
block (Photoshop strips it on load or save), but it can *import* the splines from a `.nk` file or from PhotoCraft's
`.pcraft`/JSON export. Round-tripping a PhotoCraft PSD through Photoshop therefore ends in a plain mask unless the
user re-imports. State this plainly in the plugin's help.

### 5.5 History and undo

Photoshop's History panel records *applications* (writes to the mask), one state per "Apply" or per auto-apply
commit. Spline edits inside the panel (drags, point deletes) use a panel-local undo stack of mask states, which is
cheap because the state is a small JSON. Do not try to put every drag in Photoshop's history.

### 5.6 Coordinates and colour

Photoshop pixel space is y-down, origin top-left, same as PhotoCraft's document space. Layer masks are positioned
relative to the document; read the layer's bounds and mask bounds and evaluate over the *document* rect, writing the
intersection. The mask is grayscale whatever the document's colour mode; 16-bit and 32-bit documents take 16-bit and
float planes from `getPixels` / `putLayerMask` (`componentSize`), so `pcroto_eval` should offer a 16-bit variant
before 16-bit documents are supported properly (v1: quantize to 8-bit and say so).

---

## 6. The Rust core for the plugin

New crate `crates/roto-ffi` (`crate-type = ["cdylib", "staticlib"]`), registered in `xtask/src/layers.rs`. It may
depend only on lower layers, so it sits above `vector`, `doc` and the extracted crates below. Expected binary size is
small (the evaluator, `kurbo` for freehand fitting, `serde_json`), but **measure it**: the Marketplace limit is 50 MB
for the whole `.ccx` across three architectures.

The same crates compile to `wasm32` for option B. Export the same `pcroto_*` functions from a `wasm32-unknown-unknown`
build (plain C-style exports, no `wasm-bindgen`), so a single JS wrapper can drive either the addon or the wasm module.

---

## 7. Refactors in this repository before the plugin (Phase 0)

None of these change PhotoCraft's behaviour; each is guarded by the existing roto tests (103 across
`doc`, `vector`, `io`, `engine`, `ui-egui` and `gpu`, counted 2026-10-07). Do them first, as separate commits.

1. **`photocraft-roto-ops` (L2): pure edit operations.** Move the *bodies* of `crates/engine/src/roto_cmds.rs`
   (parse params, mutate a `RotoMask`, validate, return JSON) into a crate with no `Session`:
   `fn apply(mask: &mut RotoMask, cmd: &str, params: &Value) -> Result<Value>`. The engine keeps registration, the
   layer lookup and `Session::edit` (history), and calls `apply`. Rollback on a failed validation is a clone and
   restore here, as `with_roto` does today.
2. **`photocraft-roto-edit` (L2): hit testing and gestures.** Move `crates/ui-egui/src/roto_edit.rs` (already
   free of egui types) so the app and the plugin share one implementation of "what is under the pointer".
3. **`photocraft-roto-exchange` (L2): Nuke import and export.** Move `crates/io/src/nuke.rs` (it needs only `doc`).
   `photocraft-io` re-exports it so nothing else changes.
4. **`photocraft-roto-ffi`** as above.
5. **Mask hash and bake helpers** (`bake`, `hash_mask`, the pre-bake plane codec) out of `crates/io/src/roto_map.rs`
   into `roto-ops`, so the PSD exporter and the plugin share them.

Layering is enforced by `cargo xtask layers`; add the new crates to `xtask/src/layers.rs` at L2.

---

## 8. Build, signing and release

* **Photoshop-side toolchain**: Visual Studio (Windows) and Xcode (macOS) for the C++ shim, the UXP Developer Tool for
  loading and packaging (`.ccx`) **[Adobe]**. The SDK headers come from the Adobe Developer Console; **read its licence
  before committing headers to the repo** (it is not an open-source dependency; keep it out of `THIRD_PARTY`-style
  bundling and fetch it in the build, or ask Adobe's terms).
* **Rust**: `cargo build --release -p photocraft-roto-ffi --target <triple>` for `aarch64-apple-darwin`,
  `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`. The Windows target builds on this machine; macOS needs a Mac.
* **Signing and notarization** (macOS, mandatory for the Marketplace, optional for private sharing) need an
  **Apple Developer ID** account. Decide who owns it.
* **CI**: GitHub Actions were disabled on this repository on 2026-10-06, so release builds of the plugin are either
  manual or need another runner. Three architectures on three OS runners is the usual setup; this is an open decision.
* **Distribution**: start with direct `.ccx` sharing (no marketplace review, fewer architectures allowed), move to the
  Marketplace once all three binaries are signed. Expect an admin-credentials prompt on install.

---

## 9. Spikes: run these before committing to the plan

Each has a pass/fail criterion; stop and re-plan if S1 or S2 fails.

| # | Question | How | Pass |
|---|---|---|---|
| S1 | Can anything be drawn on or interacted with on Photoshop's canvas? | Search the UXP docs and forums; ask Adobe; try a panel-to-canvas workaround | A documented API, **or** confirmation there is none (the panel viewport design stands) |
| S2 | Does a Rust `cdylib` behind a C++ shim load as a `.uxpaddon` and return a string? | Hello-world addon on Windows x64, then both macOS targets | JS gets the string; no crash on load, call, unload; panic is caught |
| S3 | Which mask-write path is correct? | Round trip a 4K gradient and a hard-edged mask through `putLayerMask` on: normal layer, fill layer, group, 8/16/32-bit, RGB/Lab | Pixel-exact (±1/255) on the default path; documented fallback for the rest |
| S4 | Does per-layer XMP survive save, close, reopen, Save As PSD, and PSD to PSB? | Write and read back a 100 KB string | Identical bytes in every case |
| S5 | Is the panel fast enough? | 4K document, 10 shapes, reduced-resolution drag, full-resolution apply | Drag under ~50 ms per frame at 1/4 scale; apply under ~2 s |
| S6 | Does the wasm build (option B) run on the current Photoshop? | Load a Rust wasm module, call it 1000 times, repeat on the oldest and newest supported versions | No crash on any supported version |
| S7 | Marketplace feasibility | Package a signed three-architecture `.ccx`; check size | Under 50 MB, accepted by the Developer Distribution portal |

---

## 10. Phases

1. **Phase 0 (in this repo):** the refactors in §7. No new behaviour. Exit: all existing tests green, `cargo xtask layers` OK.
2. **Phase 1, proof:** S1, S2, S4, S6; a panel that shows the document and one editable bezier, backed by the wasm or
   native core. Exit: edit a shape, press Apply, see the mask in Photoshop.
3. **Phase 2, editor:** tools (select, bezier, rectangle, ellipse, freehand), tree, feather and blur, Nuke import and
   export through the clipboard, panel-local undo. Exit: parity with PhotoCraft's panel for single shapes.
4. **Phase 3, fidelity:** S3 resolved, existing-mask handling, 16-bit, XMP restore with the hash guard,
   layer-type edge cases (fill layers, groups, smart objects).
5. **Phase 4, release:** signing, notarization, three architectures, packaging, help text (including the PSD
   round-trip caveat in §5.4), Marketplace submission.

---

## 11. Risks and open decisions

| Risk or decision | Detail | Owner / next step |
|---|---|---|
| No canvas overlay | Editor lives in the panel; UX is a step below PhotoCraft's | S1; accept or find an API |
| WebAssembly instability on Photoshop 2025 **[Community]** | Option B may be unusable on current versions | S6; keep A as the product |
| Mask write bugs **[Community]** | `putLayerMask` problems on some layer types | S3; selection-based fallback |
| Three signed architectures | Needs a Mac for builds and notarization, an Apple Developer ID, a Windows machine | Decide the build and signing setup |
| Windows on ARM | Not a supported hybrid target **[Adobe]** | Say so in the plugin's requirements |
| SDK licence | Adobe's headers are not open source | Read the terms before vendoring |
| CI | GitHub Actions disabled in this repo | Choose another runner or release by hand |
| Photoshop strips our `PcRM` block | PhotoCraft to Photoshop to PhotoCraft loses the splines unless re-imported | Document; maybe an XMP copy written by PhotoCraft too (cheap, worth doing: PhotoCraft could write the same XMP so Photoshop users can edit what PhotoCraft made) |
| Nuke format coverage | Opacity, blend, invert and blur attributes are not mapped on import or export yet (names unconfirmed, see the spec's section 6.1) | Needs more `.nk` samples; the plugin inherits the limit |
| 16/32-bit masks | v1 quantizes to 8-bit | Add 16-bit evaluation output |

---

## 12. Sources

Adobe documentation:
* [UXP hybrid plugins (guide)](https://developer.adobe.com/uxp/guides/how-to/hybrid-plugins/)
* [Build a hybrid plugin](https://developer.adobe.com/uxp/guides/how-to/hybrid-plugins/build)
* [Hybrid plugins FAQ](https://developer.adobe.com/uxp/guides/how-to/hybrid-plugins/faq)
* [Hybrid plugin distribution (Photoshop)](https://developer.adobe.com/photoshop/uxp/2022/guides/hybrid-plugins/distribute/)
* [Photoshop Imaging API](https://developer.adobe.com/photoshop/uxp/2022/ps-reference/media/imaging)
* [UXP manifest v5 (Photoshop)](https://developer.adobe.com/photoshop/uxp/2022/guides/uxp-guide/uxp-misc/manifest-v5/)
* [Packaging a plugin](https://developer.adobe.com/photoshop/uxp/guides/distribution/packaging-your-plugin/)
* [HTMLCanvasElement in UXP](https://developer.adobe.com/photoshop/uxp/2022/uxp-api/reference-js/global-members/html-elements/html-canvas-element)
* [Adobe Developer Console (SDK downloads)](https://developer.adobe.com/console/servicesandapis)

Community reports (forums; check against your target versions):
* [Photoshop 2025 crashes when a UXP plugin uses WASM](https://forums.creativeclouddeveloper.com/t/when-my-uxp-plugin-uses-wasm-photoshop-2025-crashes/8619)
* [Creating layer masks from pixel data in UXP](https://forums.creativeclouddeveloper.com/t/how-to-create-photoshop-layer-masks-from-pixel-data-in-uxp-plugin-lab-color-mode/11648)
* [Imaging API putLayerMask bug](https://forums.creativeclouddeveloper.com/t/imaging-api-putlayermask-bug/6582)
* [Storing custom data in a PSD (XMP)](https://forums.creativeclouddeveloper.com/t/how-to-store-retrieve-custom-data-in-psd/6121/2)
* [Best way to store document or layer specific data](https://forums.creativeclouddeveloper.com/t/best-way-to-store-document-layer-specific-data/4756)
* [UXP UI for a C++ filter plugin](https://forums.creativeclouddeveloper.com/t/uxp-ui-for-c-filter-plugin/3945)
* [Hybrid plugin walk-through (third party, unverified details)](https://mapsoft.com/posts/photoshop-hybrid-plugins.html)

This repository:
* `docs/superpowers/specs/2026-10-06-roto-mask-design.md` (the feature), `docs/superpowers/plans/2026-10-06-roto-mask.md` (how it was built)
* `crates/doc/src/roto.rs`, `crates/vector/src/roto/`, `crates/io/src/nuke.rs`, `crates/io/src/roto_map.rs`,
  `crates/engine/src/roto_cmds.rs`, `crates/ui-egui/src/roto_edit.rs`
