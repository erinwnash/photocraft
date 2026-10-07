//! GPU backend for roto masks. The tree walk, shape geometry and every decision about what to
//! compute are the CPU evaluator's (`photocraft_vector::roto::run`); this module supplies the
//! executor whose per-pixel primitives (box blur, combine, finish) run as compute passes over
//! `f32` planes kept in storage buffers, so only each shape's coverage plane goes up and the
//! finished mask comes back. The arithmetic mirrors `CpuExecutor`, so the planes agree to float
//! rounding (the parity tests hold them to 1/255).
//!
//! Anything that goes wrong (a plane too big for the device, a failed readback, a panic inside
//! wgpu) makes [`RotoGpu`] decline, and the caller uses the CPU result: a GPU problem can slow a
//! mask down but never change or break it.

use std::panic::AssertUnwindSafe;

use photocraft_doc::RotoMask;
use photocraft_doc::roto::BlendOp;
use photocraft_geom::Rect;
use photocraft_vector::roto::{Accelerator, Executor, box_radius, prepare, run};

const SHADER: &str = include_str!("roto.wgsl");
const PARAMS_BYTES: u64 = 48;

/// A wgpu device with the roto compute pipelines, usable as a roto [`Accelerator`].
pub struct RotoGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    blur: wgpu::ComputePipeline,
    combine: wgpu::ComputePipeline,
    finish: wgpu::ComputePipeline,
    max_pixels: usize,
}

impl RotoGpu {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("pc_roto"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pc_roto_layout"),
            entries: &[
                entry(0, wgpu::BufferBindingType::Uniform),
                entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pc_roto_pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |name: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let (blur, combine, finish) = (pipeline("blur_pass"), pipeline("combine"), pipeline("finish"));
        let limits = device.limits();
        let bytes = limits.max_buffer_size.min(limits.max_storage_buffer_binding_size);
        let max_pixels = usize::try_from(bytes / 4).unwrap_or(usize::MAX);
        RotoGpu { device, queue, layout, blur, combine, finish, max_pixels }
    }

    /// The largest plane (in pixels) this device can hold in one storage buffer.
    pub fn max_pixels(&self) -> usize {
        self.max_pixels
    }
}

impl Accelerator for RotoGpu {
    fn evaluate(&self, mask: &RotoMask, rect: Rect) -> Option<Vec<f32>> {
        let n = prepare(mask, rect).ok()?;
        if n > self.max_pixels {
            return None;
        }
        let out = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut ex = GpuExecutor { gpu: self, pixels: n, w: rect.width(), h: rect.height(), failed: false };
            let v = run(mask, rect, &mut ex);
            (!ex.failed && v.len() == n).then_some(v)
        }));
        out.ok().flatten()
    }
}

/// A plane on the GPU.
pub struct GpuPlane {
    buf: wgpu::Buffer,
}

struct GpuExecutor<'a> {
    gpu: &'a RotoGpu,
    pixels: usize,
    w: u32,
    h: u32,
    /// Set when a plane did not fit or a readback failed; the walk finishes with dummy data and
    /// the evaluator reports "declined".
    failed: bool,
}

#[derive(Clone, Copy, Default)]
struct Params {
    w: u32,
    h: u32,
    gw: u32,
    gh: u32,
    dx: u32,
    dy: u32,
    op: u32,
    invert: u32,
    opacity: f32,
    density: f32,
    r: u32,
    axis: u32,
}

impl Params {
    fn bytes(self) -> [u8; PARAMS_BYTES as usize] {
        let words =
            [self.w, self.h, self.gw, self.gh, self.dx, self.dy, self.op, self.invert, self.opacity.to_bits(), self.density.to_bits(), self.r, self.axis];
        let mut out = [0u8; PARAMS_BYTES as usize];
        for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *chunk = word.to_le_bytes();
        }
        out
    }
}

fn op_code(op: BlendOp) -> u32 {
    match op {
        BlendOp::Union => 0,
        BlendOp::Subtract => 1,
        BlendOp::Intersect => 2,
        BlendOp::Max => 3,
        BlendOp::Min => 4,
        BlendOp::Multiply => 5,
        BlendOp::Difference => 6,
    }
}

fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

impl GpuExecutor<'_> {
    fn storage(&self, pixels: usize, label: &str) -> Option<wgpu::Buffer> {
        if pixels > self.gpu.max_pixels {
            return None;
        }
        Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: pixels as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }))
    }

    /// Records one compute dispatch reading `src` and writing `dst`.
    fn dispatch(
        &self,
        enc: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        params: Params,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        groups: (u32, u32),
    ) {
        let uniform = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pc_roto_params"),
            size: PARAMS_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.gpu.queue.write_buffer(&uniform, 0, &params.bytes());
        let bind = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pc_roto_bind"),
            layout: &self.gpu.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: dst.as_entire_binding() },
            ],
        });
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("pc_roto"), timestamp_writes: None });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(groups.0, groups.1, 1);
    }

    fn grid(&self) -> (u32, u32) {
        (self.w.div_ceil(8), self.h.div_ceil(8))
    }
}

impl Executor for GpuExecutor<'_> {
    type Acc = Option<GpuPlane>;

    fn new_acc(&mut self) -> Option<GpuPlane> {
        let buf = self.storage(self.pixels, "pc_roto_acc");
        if buf.is_none() {
            self.failed = true;
        }
        buf.map(|buf| GpuPlane { buf })
    }

    fn combine_shape(&mut self, acc: &mut Option<GpuPlane>, raw: &[f32], raw_rect: Rect, rect: Rect, blur: f32, invert: bool, op: BlendOp, opacity: f32) {
        let Some(acc) = acc.as_ref().filter(|_| !self.failed) else { return };
        let (gw, gh) = (raw_rect.width(), raw_rect.height());
        let (dx, dy) = (rect.x0.saturating_sub(raw_rect.x0).max(0) as u32, rect.y0.saturating_sub(raw_rect.y0).max(0) as u32);
        let n = gw as usize * gh as usize;
        let (Some(a), true) = (self.storage(n, "pc_roto_shape"), raw.len() == n) else {
            self.failed = true;
            return;
        };
        self.gpu.queue.write_buffer(&a, 0, &f32_bytes(raw));
        let mut enc = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_roto_shape") });
        let mut latest = &a;
        let spare;
        let r = box_radius(blur);
        if blur > 0.0 && r > 0 {
            let Some(b) = self.storage(n, "pc_roto_blur") else {
                self.failed = true;
                return;
            };
            spare = b;
            let base = Params { w: self.w, h: self.h, gw, gh, r: r as u32, ..Default::default() };
            // Three box passes along the rows, then three along the columns, ping-ponging.
            let (mut from, mut to) = (&a, &spare);
            for axis in [0u32, 1] {
                let lines = if axis == 0 { gh } else { gw };
                for _ in 0..3 {
                    self.dispatch(&mut enc, &self.gpu.blur, Params { axis, ..base }, from, to, (lines.div_ceil(64), 1));
                    std::mem::swap(&mut from, &mut to);
                }
            }
            latest = from;
        }
        let params = Params { w: self.w, h: self.h, gw, gh, dx, dy, op: op_code(op), invert: u32::from(invert), opacity, ..Default::default() };
        self.dispatch(&mut enc, &self.gpu.combine, params, latest, &acc.buf, self.grid());
        self.gpu.queue.submit(Some(enc.finish()));
    }

    fn combine_acc(&mut self, dst: &mut Option<GpuPlane>, src: Option<GpuPlane>, op: BlendOp, opacity: f32) {
        let (Some(dst), Some(src)) = (dst.as_ref(), src.as_ref()) else { return };
        if self.failed {
            return;
        }
        let mut enc = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_roto_group") });
        let params = Params { w: self.w, h: self.h, gw: self.w, gh: self.h, op: op_code(op), opacity, ..Default::default() };
        self.dispatch(&mut enc, &self.gpu.combine, params, &src.buf, &dst.buf, self.grid());
        self.gpu.queue.submit(Some(enc.finish()));
    }

    fn finish(&mut self, acc: Option<GpuPlane>, density: f32, invert: bool) -> Vec<f32> {
        let Some(acc) = acc.filter(|_| !self.failed) else {
            self.failed = true;
            return Vec::new();
        };
        let size = self.pixels as u64 * 4;
        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pc_roto_readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_roto_finish") });
        let params = Params { w: self.w, h: self.h, gw: self.w, gh: self.h, invert: u32::from(invert), density, ..Default::default() };
        // `finish` edits in place and reads no source; the layout still wants something in that slot,
        // and it must not be the plane itself (one buffer cannot be read-only and writable at once).
        let Some(unused) = self.storage(1, "pc_roto_unused") else {
            self.failed = true;
            return Vec::new();
        };
        self.dispatch(&mut enc, &self.gpu.finish, params, &unused, &acc.buf, self.grid());
        enc.copy_buffer_to_buffer(&acc.buf, 0, &staging, 0, size);
        self.gpu.queue.submit(Some(enc.finish()));
        staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        if self.gpu.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).is_err() {
            self.failed = true;
            return Vec::new();
        }
        let Ok(data) = staging.slice(..).get_mapped_range() else {
            self.failed = true;
            return Vec::new();
        };
        let out: Vec<f32> = data.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        drop(data);
        staging.unmap();
        out
    }
}
