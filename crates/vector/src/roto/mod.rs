//! Roto mask evaluator: a tree of bezier shapes and groups (see `photocraft_doc::roto`) to an
//! alpha coverage array. Spec: `docs/superpowers/specs/2026-10-06-roto-mask-design.md`.

mod blend;
mod blur;
mod curve;
mod dispatch;
mod eval;
mod feather;
#[cfg(test)]
mod tests;

pub use blend::blend;
pub use blur::box_radius;
pub use dispatch::{AUTO_MIN_PIXELS, Accelerator, editing_layer, has_accelerator, is_editing, roto_values_auto, set_accelerator, set_editing_layer};
pub use eval::{CpuExecutor, Executor, MAX_PIXELS, prepare, roto_values, run};
