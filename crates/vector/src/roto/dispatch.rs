//! Choosing where a roto mask is evaluated. The CPU evaluator is the reference; an
//! [`Accelerator`] (the GPU backend, registered by the app once it has a device) may evaluate the
//! same mask faster. `RotoMask::backend` picks: `Cpu` never uses it, `Gpu` always tries it, `Auto`
//! tries it for masks big enough to repay a GPU round trip. An accelerator that declines, fails
//! or returns the wrong amount of data is ignored and the CPU answer is used, so an unavailable
//! or misbehaving GPU can never break a mask.

use std::sync::{Arc, RwLock};

use photocraft_doc::RotoMask;
use photocraft_doc::roto::Backend;
use photocraft_geom::Rect;

use super::eval::{prepare, roto_values};

/// An accelerated evaluator: the mask over `rect` (row-major, `0..=1`, exactly
/// `width * height` values), or `None` to have the CPU do it.
pub trait Accelerator: Send + Sync {
    fn evaluate(&self, mask: &RotoMask, rect: Rect) -> Option<Vec<f32>>;
}

static ACCELERATOR: RwLock<Option<Arc<dyn Accelerator>>> = RwLock::new(None);

/// Registers (or with `None` removes) the process-wide accelerator.
pub fn set_accelerator(a: Option<Arc<dyn Accelerator>>) {
    if let Ok(mut g) = ACCELERATOR.write() {
        *g = a;
    }
}

/// Whether an accelerator is registered.
pub fn has_accelerator() -> bool {
    ACCELERATOR.read().is_ok_and(|g| g.is_some())
}

static EDITING: RwLock<Option<photocraft_doc::LayerId>> = RwLock::new(None);

/// The layer whose roto mask is being edited, if any. While a layer is being edited its roto
/// mask is not applied to its pixels: the editor shows the whole image with the mask as an
/// overlay instead, so the user can see what they are tracing. Set by the `view.rotoEdit`
/// command (view state: never saved, never in history); the compositors consult it.
pub fn editing_layer() -> Option<photocraft_doc::LayerId> {
    EDITING.read().ok().and_then(|g| *g)
}

/// Whether `layer`'s roto mask is being edited (and so is not applied to its pixels).
pub fn is_editing(layer: photocraft_doc::LayerId) -> bool {
    editing_layer() == Some(layer)
}

/// Sets (or with `None` clears) the layer being edited.
pub fn set_editing_layer(layer: Option<photocraft_doc::LayerId>) {
    if let Ok(mut g) = EDITING.write() {
        *g = layer;
    }
}

/// `Backend::Auto` uses the accelerator from this many pixels up (below it a round trip to the
/// GPU costs more than the CPU takes) and only when some shape is blurred: measured on a 4K frame,
/// the accelerator is about 10x faster than the CPU for blur and no faster without it, because
/// shape geometry stays on the CPU for both backends.
pub const AUTO_MIN_PIXELS: usize = 1 << 20;

fn has_blur(g: &photocraft_doc::roto::Group) -> bool {
    use photocraft_doc::roto::Node;
    g.children.iter().any(|n| match n {
        Node::Shape(s) => s.visible && s.blur > 0.0,
        Node::Group(c) => c.visible && has_blur(c),
    })
}

/// Coverage of `m` over `rect`, on the accelerator when `m.backend` and the size call for it and
/// one is registered and succeeds, otherwise on the CPU ([`roto_values`]).
pub fn roto_values_auto(m: &RotoMask, rect: Rect) -> Vec<f32> {
    let pixels = rect.width() as usize * rect.height() as usize;
    let wanted = match m.backend {
        Backend::Cpu => false,
        Backend::Gpu => true,
        Backend::Auto => pixels >= AUTO_MIN_PIXELS && has_blur(&m.root),
    };
    if wanted && prepare(m, rect).is_ok() {
        let accel = ACCELERATOR.read().ok().and_then(|g| g.clone());
        if let Some(v) = accel.and_then(|a| a.evaluate(m, rect)).filter(|v| v.len() == pixels) {
            return v;
        }
    }
    roto_values(m, rect)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// The accelerator is process-wide: tests that register one run one at a time.
    static LOCK: Mutex<()> = Mutex::new(());

    struct Fake {
        calls: AtomicUsize,
        answer: fn(usize) -> Option<Vec<f32>>,
    }

    impl Accelerator for Fake {
        fn evaluate(&self, _: &RotoMask, rect: Rect) -> Option<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (self.answer)(rect.width() as usize * rect.height() as usize)
        }
    }

    fn with_fake<R>(answer: fn(usize) -> Option<Vec<f32>>, f: impl FnOnce(&Fake) -> R) -> R {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fake = Arc::new(Fake { calls: AtomicUsize::new(0), answer });
        set_accelerator(Some(fake.clone()));
        let r = f(&fake);
        set_accelerator(None);
        r
    }

    fn mask(backend: Backend) -> RotoMask {
        RotoMask { backend, ..Default::default() }
    }

    const SMALL: Rect = Rect { x0: 0, y0: 0, x1: 16, y1: 16 };
    const BIG: Rect = Rect { x0: 0, y0: 0, x1: 1024, y1: 1024 };

    #[test]
    fn cpu_never_uses_the_accelerator_and_gpu_always_tries_it() {
        with_fake(
            |n| Some(vec![0.5; n]),
            |fake| {
                assert!(roto_values_auto(&mask(Backend::Cpu), SMALL).iter().all(|v| *v == 1.0), "CPU: an empty mask reveals everything");
                assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
                assert!(roto_values_auto(&mask(Backend::Gpu), SMALL).iter().all(|v| *v == 0.5), "GPU: the accelerator's answer");
                assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
            },
        );
    }

    fn blurred(backend: Backend, visible: bool) -> RotoMask {
        use photocraft_doc::roto::{Node, NodeId, Shape};
        let mut s = Shape::new(NodeId(1), "s");
        s.blur = 8.0;
        s.visible = visible;
        let mut m = mask(backend);
        m.root.children.push(Node::Shape(s));
        m
    }

    #[test]
    fn auto_uses_it_only_for_big_masks_that_blur() {
        with_fake(
            |n| Some(vec![0.5; n]),
            |fake| {
                let calls = || fake.calls.load(Ordering::SeqCst);
                assert!(roto_values_auto(&blurred(Backend::Auto, true), SMALL).iter().all(|v| *v == 1.0));
                assert_eq!(calls(), 0, "too small to repay a GPU round trip");
                assert!(roto_values_auto(&mask(Backend::Auto), BIG).iter().all(|v| *v == 1.0));
                assert_eq!(calls(), 0, "big but nothing is blurred: the GPU is no faster");
                assert!(roto_values_auto(&blurred(Backend::Auto, false), BIG).iter().all(|v| *v == 1.0));
                assert_eq!(calls(), 0, "a hidden blurred shape does not count");
                assert!(roto_values_auto(&blurred(Backend::Auto, true), BIG).iter().all(|v| *v == 0.5));
                assert_eq!(calls(), 1, "big and blurred");
            },
        );
    }

    #[test]
    fn a_declining_or_misbehaving_accelerator_falls_back_to_the_cpu() {
        for answer in [(|_| None) as fn(usize) -> Option<Vec<f32>>, |n| Some(vec![0.5; n + 1]), |_| Some(Vec::new())] {
            with_fake(answer, |fake| {
                let v = roto_values_auto(&mask(Backend::Gpu), SMALL);
                assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
                assert_eq!(v, roto_values(&mask(Backend::Gpu), SMALL), "the CPU answer is used");
                assert_eq!(v.len(), 256);
            });
        }
    }

    #[test]
    fn empty_rects_and_invalid_masks_never_reach_the_accelerator() {
        with_fake(
            |n| Some(vec![0.5; n]),
            |fake| {
                assert!(roto_values_auto(&mask(Backend::Gpu), Rect::EMPTY).is_empty());
                let mut bad = mask(Backend::Gpu);
                bad.density = 7.0;
                assert!(roto_values_auto(&bad, SMALL).iter().all(|v| *v == 0.0));
                assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
            },
        );
    }

    #[test]
    fn without_an_accelerator_every_backend_is_the_cpu() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_accelerator(None);
        assert!(!has_accelerator());
        for b in [Backend::Auto, Backend::Cpu, Backend::Gpu] {
            assert_eq!(roto_values_auto(&mask(b), SMALL), roto_values(&mask(b), SMALL));
        }
    }

    #[test]
    fn the_editing_layer_is_process_wide_state_with_one_owner() {
        use photocraft_doc::LayerId;
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_editing_layer(None);
        assert_eq!(editing_layer(), None);
        assert!(!is_editing(LayerId(7)));
        set_editing_layer(Some(LayerId(7)));
        assert!(is_editing(LayerId(7)) && !is_editing(LayerId(8)));
        set_editing_layer(Some(LayerId(8)));
        assert!(is_editing(LayerId(8)) && !is_editing(LayerId(7)), "one layer at a time");
        set_editing_layer(None);
        assert_eq!(editing_layer(), None);
    }
}
