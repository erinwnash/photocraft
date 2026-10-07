use photocraft_doc::roto::BlendOp;

/// Combines `b` (the new contribution) into `a` (the accumulator beneath), both `0..=1`.
pub fn blend(op: BlendOp, a: f32, b: f32) -> f32 {
    match op {
        BlendOp::Union => a + b - a * b,
        BlendOp::Subtract => a * (1.0 - b),
        BlendOp::Intersect | BlendOp::Min => a.min(b),
        BlendOp::Max => a.max(b),
        BlendOp::Multiply => a * b,
        BlendOp::Difference => (a - b).abs(),
    }
}
