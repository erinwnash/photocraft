/// The box half-width of each of the three passes that approximate a Gaussian of `sigma = radius`
/// (box widths whose three-pass variance matches sigma²: Wells 1986 / Kovesi). Zero means the
/// blur is a no-op. Shared with accelerated backends so they use the same widths.
pub fn box_radius(radius: f32) -> usize {
    if !(radius.is_finite() && radius > 0.0) {
        return 0;
    }
    let ideal = (12.0 * radius * radius / 3.0 + 1.0).sqrt();
    (((ideal.floor() as usize) | 1).max(1) - 1) / 2
}

/// Approximate Gaussian blur of a `w`×`h` plane with `sigma = radius`: three box passes per axis,
/// edges clamped (the caller grows the plane so edges are constant). The same scheme as the
/// mask feather in `photocraft-compose`, which sits in a higher layer and cannot be shared.
pub(crate) fn blur_plane(v: &mut [f32], w: usize, h: usize, radius: f32) {
    let r = box_radius(radius);
    if r == 0 || w == 0 || h == 0 || v.len() < w * h {
        return;
    }
    let mut line = Vec::new();
    let mut pass = |v: &mut [f32], len: usize, count: usize, at: &dyn Fn(usize, usize) -> usize| {
        for k in 0..count {
            for _ in 0..3 {
                line.clear();
                line.extend((0..len).map(|i| v[at(k, i)]));
                let get = |i: isize| line[i.clamp(0, len as isize - 1) as usize];
                let mut acc: f32 = (-(r as isize)..=r as isize).map(get).sum();
                let n = (2 * r + 1) as f32;
                for i in 0..len {
                    v[at(k, i)] = acc / n;
                    acc += get(i as isize + r as isize + 1) - get(i as isize - r as isize);
                }
            }
        }
    };
    pass(v, w, h, &|row, i| row * w + i);
    pass(v, h, w, &|col, i| i * w + col);
}
