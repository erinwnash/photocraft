//! Roto masks in PSD files.
//!
//! Photoshop has no roto mask, so on export the roto result is *baked* into the layer's ordinary
//! raster mask (the product of any existing pixel mask and the roto mask), which is exactly what
//! Photoshop shows. PhotoCraft also writes a private `PcRM` layer block holding the editable
//! splines, so a PhotoCraft → PSD → PhotoCraft trip does not lose them:
//!
//! ```text
//! [version u8 = 1] [json length u32 LE] [json: Meta] [zlib: the pre-bake pixel mask plane, u8]
//! ```
//!
//! On import the splines are restored only while the layer's mask is still *exactly* the one that
//! was exported (a hash of its quantized values over the canvas plus its flags); then the mask is
//! returned to its pre-bake state (none, or the pixel mask with its density and feather). If the
//! mask was edited elsewhere, or the block is damaged, the baked mask stays as an ordinary mask
//! and a warning says so. The block is untrusted input: sizes are capped and nothing panics.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Layer, LayerMask, RotoMask};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};

/// Key of the private layer block.
pub const KEY: [u8; 4] = *b"PcRM";
const VERSION: u8 = 1;
/// Largest spline description accepted.
const MAX_JSON: usize = 64 << 20;
/// Largest pre-bake plane accepted (pixels).
const MAX_PLANE: usize = 1 << 28;

#[derive(Serialize, Deserialize)]
struct Pre {
    density: f32,
    feather: f32,
    enabled: bool,
    linked: bool,
    default: f32,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    roto: RotoMask,
    /// The roto mask was multiplied into the exported layer mask.
    baked: bool,
    /// Hash of the layer mask as exported (`None`: the layer had no mask).
    mask_hash: Option<u64>,
    /// The pixel mask before baking, when there was one (its plane follows the JSON).
    pre: Option<Pre>,
}

fn q255(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn plane(m: &LayerMask, area: Rect) -> Vec<u8> {
    m.surface.read_region(area).iter().map(|v| q255(*v)).collect()
}

/// Identity of a mask: flags, density, feather, default and every pixel over the canvas, each
/// quantized to the 8 bits a PSD mask channel keeps at minimum.
fn hash_mask(m: &LayerMask, area: Rect) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut put = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    put(u8::from(m.enabled));
    put(u8::from(m.linked));
    put(q255(m.density));
    m.feather.to_bits().to_le_bytes().into_iter().for_each(&mut put);
    put(q255(m.surface.default_pixel().first().copied().unwrap_or(1.0)));
    plane(m, area).into_iter().for_each(&mut put);
    h
}

/// The mask Photoshop will see: the existing pixel mask (with its density) times the roto mask,
/// as one plain mask over the canvas.
pub fn bake(existing: Option<&LayerMask>, roto: &RotoMask, canvas: Rect, mask_fmt: PixelFormat) -> LayerMask {
    let mut values = photocraft_vector::roto::roto_values(roto, canvas);
    if let Some(m) = existing {
        let mut pm = Vec::new();
        m.values_into(canvas, &mut pm);
        for (a, b) in values.iter_mut().zip(&pm) {
            *a *= *b;
        }
    }
    // Outside the canvas: whatever each mask gives far away.
    let far = Rect::from_xywh(canvas.x0.saturating_sub(1_000_000), canvas.y0.saturating_sub(1_000_000), 1, 1);
    let outside = photocraft_vector::roto::roto_values(roto, far).first().copied().unwrap_or(0.0) * existing.map_or(1.0, |m| m.value(far.x0, far.y0));
    let mut surface = Surface::with_default(mask_fmt, &[outside]);
    surface.write_region(canvas, &values);
    surface.prune();
    LayerMask { surface, enabled: true, linked: roto.linked && existing.is_none_or(|m| m.linked), density: 1.0, feather: existing.map_or(0.0, |m| m.feather) }
}

/// The `PcRM` payload. `existing` is the layer's own pixel mask, `exported` the mask written to
/// the file (the baked one, or the existing one when the roto mask is disabled).
pub fn encode(roto: &RotoMask, existing: Option<&LayerMask>, exported: Option<&LayerMask>, baked: bool, canvas: Rect) -> Vec<u8> {
    let pre = (baked && existing.is_some())
        .then(|| {
            existing.map(|m| Pre {
                density: m.density,
                feather: m.feather,
                enabled: m.enabled,
                linked: m.linked,
                default: m.surface.default_pixel().first().copied().unwrap_or(1.0),
            })
        })
        .flatten();
    let meta = Meta { roto: roto.clone(), baked, mask_hash: exported.map(|m| hash_mask(m, canvas)), pre };
    let json = serde_json::to_vec(&meta).unwrap_or_default();
    let mut out = vec![VERSION];
    out.extend_from_slice(&u32::try_from(json.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&json);
    if baked && let Some(m) = existing {
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        if z.write_all(&plane(m, canvas)).is_ok()
            && let Ok(bytes) = z.finish()
        {
            out.extend_from_slice(&bytes);
        }
    }
    out
}

/// After a PSD import: restores every layer's splines from its `PcRM` block (reporting layers
/// where that is not possible) and drops the block, so a re-export writes a fresh one or none.
pub fn import_all(doc: &mut photocraft_doc::Document, warnings: &mut Vec<String>) {
    let (canvas, depth) = (doc.bounds(), doc.depth);
    let paths: Vec<_> = doc.walk().into_iter().filter(|(_, _, l)| l.psd_blocks.iter().any(|(k, _)| *k == KEY)).map(|(p, _, _)| p).collect();
    for path in paths {
        let Some(l) = doc.layer_at_mut(&path) else { continue };
        let data = l.psd_blocks.iter().find(|(k, _)| *k == KEY).map(|(_, d)| d.clone());
        l.psd_blocks.retain(|(k, _)| *k != KEY);
        if let Some(d) = data
            && let Err(why) = restore(l, &d, canvas, depth)
        {
            warnings.push(format!("layer \"{}\": roto splines not restored: {why}", l.name));
        }
    }
}

/// Restores the splines of `l` from a `PcRM` payload. `Err` says why not (the layer is left as
/// it is: a plain baked mask).
pub fn restore(l: &mut Layer, data: &[u8], canvas: Rect, depth: SampleType) -> Result<(), &'static str> {
    let (&version, rest) = data.split_first().ok_or("the block is empty")?;
    if version != VERSION {
        return Err("the block has an unknown version");
    }
    let (len, rest) = rest.split_first_chunk::<4>().ok_or("the block is truncated")?;
    let len = u32::from_le_bytes(*len) as usize;
    if len > MAX_JSON || len > rest.len() {
        return Err("the block is truncated");
    }
    let (json, tail) = rest.split_at(len);
    let meta: Meta = serde_json::from_slice(json).map_err(|_| "the block is damaged")?;
    meta.roto.validate().map_err(|_| "the splines are not valid")?;
    let current = l.mask.as_ref().map(|m| hash_mask(m, canvas));
    if current != meta.mask_hash {
        return Err("the layer mask was edited outside PhotoCraft; kept as a plain mask");
    }
    if meta.baked {
        match meta.pre {
            None => l.mask = None,
            Some(p) => {
                let n = (canvas.width() as usize).checked_mul(canvas.height() as usize).filter(|n| *n <= MAX_PLANE).ok_or("the canvas is too large")?;
                let mut raw = Vec::with_capacity(n);
                ZlibDecoder::new(tail).take(n as u64 + 1).read_to_end(&mut raw).map_err(|_| "the saved mask is damaged")?;
                if raw.len() != n {
                    return Err("the saved mask does not match the canvas");
                }
                let mut surface = Surface::with_default(PixelFormat::new(ColorMode::Grayscale, depth, false), &[p.default]);
                let values: Vec<f32> = raw.iter().map(|b| f32::from(*b) / 255.0).collect();
                surface.write_region(canvas, &values);
                surface.prune();
                l.mask = Some(LayerMask { surface, enabled: p.enabled, linked: p.linked, density: p.density.clamp(0.0, 1.0), feather: p.feather.max(0.0) });
            }
        }
    }
    l.roto_mask = Some(meta.roto);
    Ok(())
}
