//! The GPU roto backend against the CPU oracle. Both run the same tree walk; only the blur,
//! combine and finish primitives differ, so the planes must agree to within 1/255 (in practice
//! they agree to float rounding). Skips on machines without a usable adapter.

use std::sync::Arc;

use photocraft_doc::RotoMask;
use photocraft_doc::roto::{Backend, BlendOp, Falloff, Group, Node, NodeId, OverlapMode, Point, PointId, Shape, V2};
use photocraft_geom::Rect;
use photocraft_gpu::roto::RotoGpu;
use photocraft_vector::roto::{Accelerator, roto_values, roto_values_auto, set_accelerator};

fn gpu() -> Option<RotoGpu> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    Some(RotoGpu::new(device, queue))
}

fn square(id: u64, x0: f64, y0: f64, x1: f64, y1: f64) -> Shape {
    let mut s = Shape::new(NodeId(id), &format!("s{id}"));
    for (i, (x, y)) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].into_iter().enumerate() {
        s.points.push(Point::corner(PointId(id * 10 + i as u64), x, y));
    }
    s
}

fn feather(mut s: Shape, d: f64) -> Shape {
    let cx = s.points.iter().map(|p| p.pos.x).sum::<f64>() / s.points.len() as f64;
    let cy = s.points.iter().map(|p| p.pos.y).sum::<f64>() / s.points.len() as f64;
    for p in &mut s.points {
        p.feather_pos = V2::new(if p.pos.x < cx { -d } else { d }, if p.pos.y < cy { -d } else { d });
    }
    s
}

fn mask(shapes: Vec<Shape>) -> RotoMask {
    let mut m = RotoMask::default();
    m.root.children = shapes.into_iter().map(Node::Shape).collect();
    m
}

/// Worst per-pixel difference between the CPU oracle and the GPU for `m` over `rect`.
fn worst(g: &RotoGpu, m: &RotoMask, rect: Rect, what: &str) -> f32 {
    let cpu = roto_values(m, rect);
    let out = g.evaluate(m, rect).unwrap_or_else(|| panic!("{what}: the GPU declined"));
    assert_eq!(out.len(), cpu.len(), "{what}: length");
    let d = cpu.iter().zip(&out).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    println!("{what}: max diff {d:.6} ({:.3}/255)", d * 255.0);
    assert!(d <= 1.0 / 255.0, "{what}: GPU differs from the CPU by {:.2}/255", d * 255.0);
    d
}

const R: Rect = Rect { x0: 0, y0: 0, x1: 120, y1: 100 };

#[test]
fn hard_feathered_and_inward_shapes_match() {
    let Some(g) = gpu() else { return };
    worst(&g, &mask(vec![square(1, 20.0, 20.0, 90.0, 70.0)]), R, "hard square");
    for (falloff, name) in [(Falloff::Linear, "linear"), (Falloff::Smooth, "smooth"), (Falloff::EaseIn, "easeIn"), (Falloff::EaseOut, "easeOut")] {
        let mut s = feather(square(1, 30.0, 25.0, 85.0, 70.0), 9.0);
        s.falloff = falloff;
        worst(&g, &mask(vec![s]), R, &format!("outward feather {name}"));
    }
    let inward = feather(square(1, 20.0, 15.0, 100.0, 85.0), -12.0);
    worst(&g, &mask(vec![inward]), R, "inward feather");
}

#[test]
fn overlap_modes_and_shape_settings_match() {
    let Some(g) = gpu() else { return };
    let fold = feather(square(1, 30.0, 30.0, 70.0, 70.0), -25.0);
    for mode in [OverlapMode::Max, OverlapMode::Sum, OverlapMode::Over] {
        let mut m = mask(vec![fold.clone()]);
        m.overlap = mode;
        worst(&g, &m, R, &format!("fold {mode:?}"));
    }
    let mut a = square(1, 10.0, 10.0, 80.0, 70.0);
    a.opacity = 0.7;
    let mut b = feather(square(2, 40.0, 30.0, 110.0, 90.0), 6.0);
    b.invert = true;
    b.opacity = 0.6;
    let mut m = mask(vec![a, b]);
    m.density = 0.8;
    m.invert = true;
    worst(&g, &m, R, "opacity, invert, density");
}

#[test]
fn every_blend_op_matches() {
    let Some(g) = gpu() else { return };
    for op in [BlendOp::Union, BlendOp::Subtract, BlendOp::Intersect, BlendOp::Max, BlendOp::Min, BlendOp::Multiply, BlendOp::Difference] {
        let a = feather(square(1, 10.0, 10.0, 80.0, 70.0), 5.0);
        let mut b = feather(square(2, 40.0, 30.0, 110.0, 90.0), 8.0);
        b.blend_op = op;
        b.opacity = 0.85;
        worst(&g, &mask(vec![a, b]), R, &format!("blend {op:?}"));
    }
}

#[test]
fn blur_matches_including_big_radii_and_offset_rects() {
    let Some(g) = gpu() else { return };
    for blur in [1.0, 4.5, 12.0, 40.0] {
        let mut s = feather(square(1, 40.0, 30.0, 80.0, 70.0), 6.0);
        s.blur = blur;
        worst(&g, &mask(vec![s]), R, &format!("blur {blur}"));
    }
    // A rect that does not start at the origin and cuts the shape: the crop offsets matter.
    let mut s = square(1, 10.0, 10.0, 90.0, 80.0);
    s.blur = 20.0;
    let m = mask(vec![s]);
    worst(&g, &m, Rect::new(-30, -20, 70, 60), "blur, offset rect");
    worst(&g, &m, Rect::new(50, 40, 150, 140), "blur, offset rect 2");
    // A blur larger than the frame itself.
    let mut s = square(1, 5.0, 5.0, 40.0, 30.0);
    s.blur = 150.0;
    worst(&g, &mask(vec![s]), Rect::new(0, 0, 50, 40), "blur larger than the frame");
}

#[test]
fn nested_groups_with_transforms_match() {
    let Some(g) = gpu() else { return };
    let mut inner = Group::new(NodeId(20), "inner");
    inner.opacity = 0.75;
    inner.blend_op = BlendOp::Subtract;
    inner.transform.translate = V2::new(6.0, -4.0);
    inner.children.push(Node::Shape(feather(square(3, 40.0, 35.0, 70.0, 60.0), 5.0)));
    let mut outer = Group::new(NodeId(10), "outer");
    outer.transform.rotate = 8.0;
    outer.transform.pivot = V2::new(60.0, 50.0);
    outer.children.push(Node::Shape(square(1, 15.0, 15.0, 100.0, 85.0)));
    outer.children.push(Node::Group(inner));
    let mut m = RotoMask::default();
    m.root.children.push(Node::Group(outer));
    m.root.transform.translate = V2::new(3.0, 2.0);
    worst(&g, &m, R, "nested groups");
    // A hidden shape and an empty group do nothing, and an empty mask is all zeros on both.
    let mut empty = RotoMask::default();
    empty.root.children.push(Node::Group(Group::new(NodeId(1), "empty")));
    worst(&g, &empty, R, "empty group");
    assert!(g.evaluate(&RotoMask::default(), R).is_some_and(|v| v.iter().all(|x| *x == 0.0)));
}

#[test]
fn the_dispatch_layer_uses_the_gpu_and_falls_back_cleanly() {
    let Some(g) = gpu() else { return };
    let g = Arc::new(g);
    let mut s = feather(square(1, 20.0, 20.0, 90.0, 70.0), 7.0);
    s.blur = 3.0;
    let mut m = mask(vec![s]);
    set_accelerator(Some(g.clone()));
    m.backend = Backend::Gpu;
    let via_gpu = roto_values_auto(&m, R);
    m.backend = Backend::Cpu;
    let via_cpu = roto_values_auto(&m, R);
    assert_eq!(via_cpu, roto_values(&m, R), "Cpu is the oracle exactly");
    let d = via_gpu.iter().zip(&via_cpu).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(d <= 1.0 / 255.0, "dispatch through the GPU differs by {:.2}/255", d * 255.0);
    // An invalid mask never reaches the GPU: zeros, like the CPU.
    let mut bad = m.clone();
    bad.backend = Backend::Gpu;
    bad.density = 5.0;
    assert!(roto_values_auto(&bad, R).iter().all(|v| *v == 0.0));
    set_accelerator(None);
}

#[test]
fn a_plane_too_big_for_the_device_is_declined_not_crashed() {
    let Some(g) = gpu() else { return };
    // One pixel more than the device can hold in a storage buffer: declined up front, with no
    // plane allocated on either side.
    let side = (g.max_pixels() as f64).sqrt().ceil() as i32 + 1;
    let m = mask(vec![square(1, 0.0, 0.0, 10.0, 10.0)]);
    assert!(g.evaluate(&m, Rect::new(0, 0, side, side)).is_none());
}

/// Timing of CPU against GPU on a full 4K-ish frame. Run with
/// `cargo test -p photocraft-gpu --test roto --release bench -- --ignored --nocapture`.
#[test]
#[ignore = "timing only"]
fn bench_cpu_against_gpu() {
    let Some(g) = gpu() else { return };
    let rect = Rect::new(0, 0, 3840, 2160);
    for (shapes, blur) in [(1usize, 0.0f32), (1, 25.0), (8, 0.0), (8, 25.0), (24, 0.0)] {
        let mut m = RotoMask::default();
        for i in 0..shapes {
            let o = i as f64 * 90.0;
            let mut s = feather(square(i as u64 + 1, 200.0 + o, 200.0 + o / 2.0, 1400.0 + o, 1200.0 + o / 2.0), 30.0);
            s.blur = blur;
            s.opacity = 0.8;
            m.root.children.push(Node::Shape(s));
        }
        let t = std::time::Instant::now();
        let cpu = roto_values(&m, rect);
        let cpu_ms = t.elapsed().as_secs_f64() * 1e3;
        let _ = g.evaluate(&m, rect); // warm-up: pipelines and buffers
        let t = std::time::Instant::now();
        let gpu_out = g.evaluate(&m, rect).expect("gpu");
        let gpu_ms = t.elapsed().as_secs_f64() * 1e3;
        let d = cpu.iter().zip(&gpu_out).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        println!("{shapes:>2} shapes, blur {blur:>4}: CPU {cpu_ms:>8.1} ms   GPU {gpu_ms:>8.1} ms   (max diff {d:.6})");
    }
}
