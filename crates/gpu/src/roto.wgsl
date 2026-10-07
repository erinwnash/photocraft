// Roto mask primitives: box blur, combine and finish over f32 planes in storage buffers. They
// mirror `photocraft_vector::roto::CpuExecutor` operation for operation (same window sums, same
// blend arithmetic), so the two backends agree to float rounding.

struct Params {
    w: u32,        // output plane (the walk's rect)
    h: u32,
    gw: u32,       // source plane (the rect grown by the blur's reach, or the same as the output)
    gh: u32,
    dx: u32,       // where the output sits inside the source plane
    dy: u32,
    op: u32,       // 0 union, 1 subtract, 2 intersect, 3 max, 4 min, 5 multiply, 6 difference
    invert: u32,
    opacity: f32,
    density: f32,
    r: u32,        // box half-width
    axis: u32,     // blur direction: 0 along rows, 1 along columns
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

// One thread per line: three box passes make a Gaussian, one pass per dispatch.
@compute @workgroup_size(64)
fn blur_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let line = id.x;
    let r = i32(p.r);
    let n = f32(2 * r + 1);
    if p.axis == 0u {
        if line >= p.gh { return; }
        let len = i32(p.gw);
        let base = line * p.gw;
        var acc: f32 = 0.0;
        for (var k = -r; k <= r; k++) {
            acc += src[base + u32(clamp(k, 0, len - 1))];
        }
        for (var i = 0; i < len; i++) {
            dst[base + u32(i)] = acc / n;
            acc += src[base + u32(clamp(i + r + 1, 0, len - 1))] - src[base + u32(clamp(i - r, 0, len - 1))];
        }
    } else {
        if line >= p.gw { return; }
        let len = i32(p.gh);
        var acc: f32 = 0.0;
        for (var k = -r; k <= r; k++) {
            acc += src[u32(clamp(k, 0, len - 1)) * p.gw + line];
        }
        for (var i = 0; i < len; i++) {
            dst[u32(i) * p.gw + line] = acc / n;
            acc += src[u32(clamp(i + r + 1, 0, len - 1)) * p.gw + line] - src[u32(clamp(i - r, 0, len - 1)) * p.gw + line];
        }
    }
}

fn blend(op: u32, a: f32, b: f32) -> f32 {
    switch op {
        case 0u: { return a + b - a * b; }
        case 1u: { return a * (1.0 - b); }
        case 2u, 4u: { return min(a, b); }
        case 3u: { return max(a, b); }
        case 5u: { return a * b; }
        default: { return abs(a - b); }
    }
}

// dst (the accumulator) = dst + (blend(op, dst, shape) - dst) * opacity, where `shape` is the
// source plane cropped to the output (offset dx, dy) and optionally inverted.
@compute @workgroup_size(8, 8)
fn combine(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    var s = src[(id.y + p.dy) * p.gw + id.x + p.dx];
    if p.invert == 1u { s = 1.0 - s; }
    let i = id.y * p.w + id.x;
    let a = dst[i];
    dst[i] = a + (blend(p.op, a, s) - a) * p.opacity;
}

// dst = clamp(dst, 0, 1) * density, inverted when asked.
@compute @workgroup_size(8, 8)
fn finish(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.w || id.y >= p.h { return; }
    let i = id.y * p.w + id.x;
    let x = clamp(dst[i], 0.0, 1.0) * p.density;
    if p.invert == 1u { dst[i] = 1.0 - x; } else { dst[i] = x; }
}
