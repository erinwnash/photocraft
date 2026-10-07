//! Roto mask evaluator: a tree of bezier shapes and groups (see `photocraft_doc::roto`) to an
//! alpha coverage array. Spec: `docs/superpowers/specs/2026-10-06-roto-mask-design.md`.

mod blend;
mod curve;
mod eval;
#[cfg(test)]
mod tests;

pub use blend::blend;
pub use eval::roto_values;
