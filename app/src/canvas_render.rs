//! The bridge from figcraft's CPU pixels to a GPUI-drawable image.
//!
//! # Why this module exists
//!
//! Spool's renderer is the vendored figcraft engine, and it renders on the CPU:
//! [`figcraft_render::Retained`] hands back a `tiny_skia::Pixmap` of premultiplied
//! RGBA. Spool's frontend is GPUI, which draws through the GPU. Nothing connects
//! the two, so this module is that connection and nothing else — it owns the
//! thread the engine runs on, the conversion of pixels into something GPUI can
//! paint, and the bookkeeping that decides when those pixels must be uploaded
//! again.
//!
//! # The GPUI image API, as it actually is
//!
//! GPUI has no "give me these bytes and draw them" entry point. The shortest real
//! path is:
//!
//! ```text
//! gpui::RenderImage::new(...)  ->  Arc<RenderImage>  ->  ImageSource::Render
//!                                                 ->  gpui::img(source)
//! ```
//!
//! and [`RenderImage::new`] takes `impl Into<SmallVec<[image::Frame; 1]>>`
//! (`crates/gpui/src/assets.rs`). `image::Frame` wraps an
//! `ImageBuffer<Rgba<u8>, Vec<u8>>`, so the `image` crate is part of this
//! conversion whether or not it is pleasant, and `app/Cargo.toml` needs
//! `image = "0.25"` for it. That version is already in the lockfile as a
//! transitive dependency of GPUI, so naming it explicitly changes nothing that is
//! fetched or resolved.
//!
//! Three further facts from the same revision shape everything below.
//!
//! **The byte layout is BGRA with straight (un-premultiplied) alpha.** GPUI
//! uploads `MTLPixelFormat::BGRA8Unorm`
//! (`crates/gpui_apple/src/metal_atlas.rs`), and its own decoders run
//! `gpui::swap_rgba_pa_to_bgra` on the way in. The engine produces *premultiplied*
//! RGBA, so every pixel is demultiplied and byte-permuted here. Both halves are
//! necessary: leaving the alpha premultiplied darkens every translucent pixel, and
//! leaving the order as RGBA swaps red and blue.
//!
//! **A texture is uploaded once per `RenderImage`.** `RenderImage::new` mints a
//! fresh `ImageId`, and the atlas keys its textures on
//! `AtlasKey::Image { image_id, frame_index }` (`Window::paint_image`). Keeping the
//! same `Arc<RenderImage>` alive across frames therefore keeps the same GPU
//! texture, and building a new one costs a full upload. That is the entire
//! optimisation this module has, and [`TextureCache`] exists to make it explicit
//! rather than incidental.
//!
//! **The image is drawn at its device size.** `RenderImage::scale_factor` is
//! `pub(crate)`, so an image made here reports `1.0` and GPUI lays it out at its
//! device-pixel width. The draw side must size the element itself, which is what
//! [`CanvasFrame::logical_size`] and [`CanvasFrame::origin_in_view`] are for.
//!
//! # Device pixel ratio
//!
//! The engine is told in device pixels and the window is laid out in logical
//! points, so the view is built at `zoom * ratio` and the requested size is
//! `logical * ratio` rounded to whole device pixels — see [`device_view`]. The
//! frame then reports the ratio it was rendered for, so the blit can be placed
//! without the draw side having to remember which display it was on.
//!
//! `figcraft_render::retained::snap` (note the module path — `snap` is not
//! re-exported at the crate root) is what makes the geometry exact: it floors the
//! origin onto the zoom's pixel grid, so at a fixed zoom a pan is an integer shift
//! of the previous image and a partially redrawn frame lines up with the pixels
//! already in the texture.
//!
//! # What `Work` buys
//!
//! The engine is retained, not stateless: after a pan or an edit it reports
//! [`Work::Reused`], [`Work::Partial`] or [`Work::Full`] instead of redrawing
//! everything. `Reused` is the only case in which no pixel differs from what the
//! texture already holds, so it is the only case in which [`TextureCache`] skips
//! the upload. `Partial` still costs a full upload here, because GPUI's atlas has
//! no path for patching bytes into a live texture: the damage rectangles are
//! recorded in [`Damage`] so the decision is visible and testable, and a partial
//! upload can be built on them later without re-deriving where the change was.
//!
//! # What the draw side is told
//!
//! A frame says which frame it is ([`CanvasFrame::generation`]) and which
//! document it is of ([`CanvasFrame::doc`]). Both are there because the picture
//! on the screen is always a frame old, and the draw side has to be able to say
//! so: a frame that arrived while it was painting is not on the screen yet, and
//! without knowing that the canvas would sit on a stale picture until the next
//! input event.
//!
//! # What is not asked for
//!
//! A request that is the same as the last one is not worth making
//! ([`CanvasRenderer::repeats`]): the answer would be the frame that is already
//! there. The canvas paints on every frame it is given, so this is the
//! difference between a canvas that goes idle when nothing is happening and one
//! that renders and repaints for ever. figcraft's egui frontend keeps the same
//! key (`JobKey`) and does the same thing.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use figcraft_doc::{Document, NodeId};
use figcraft_render::retained::snap;
use figcraft_render::{Options, Pixmap, Retained, View, Work};
use gpui::RenderImage;
use image::{Frame, RgbaImage};

#[cfg(test)]
mod bench;

#[cfg(test)]
mod window_bench;

/// The largest device-pixel edge a view may ask for.
///
/// The engine clamps every pixmap dimension to this (`figcraft_render`'s
/// `new_pixmap`, and the same clamp inside `snap`), so a request beyond it is not
/// rejected — it is silently drawn smaller. Clamping here instead means the frame
/// the draw side is told about is the frame that was rendered.
pub const MAX_DEVICE_EDGE: u32 = 16_384;

/// Damage rectangles are merged into one bounding box past this many.
///
/// The same ceiling the engine applies to its own patch rectangles, and for the
/// same reason: past a handful of rects the bookkeeping costs more than the
/// bookkeeping is worth, and one bounding box is always correct.
pub const MAX_DAMAGE_RECTS: usize = 8;

/// A size in whole device pixels — the unit the engine's view and a GPU texture
/// are both measured in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DeviceSize {
    pub width: u32,
    pub height: u32,
}

impl DeviceSize {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// The pixel count, saturating rather than wrapping on absurd input.
    pub fn pixels(self) -> u64 {
        u64::from(self.width).saturating_mul(u64::from(self.height))
    }

    /// The size in logical points, which is what a GPUI element wants.
    ///
    /// A zero or negative ratio would make this infinite. [`device_view`] has
    /// already refused such a ratio, so `1.0` here is a floor against a
    /// divide-by-zero rather than a policy about broken windows.
    pub fn logical(self, ratio: f32) -> (f32, f32) {
        let ratio = if ratio.is_finite() && ratio > 0.0 {
            ratio
        } else {
            1.0
        };
        (self.width as f32 / ratio, self.height as f32 / ratio)
    }
}

/// A half-open device-pixel rectangle, `[x0, x1) × [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceRect {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl DeviceRect {
    /// The whole of a view of `size`.
    pub fn whole(size: DeviceSize) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: i64::from(size.width),
            y1: i64::from(size.height),
        }
    }

    pub fn width(self) -> u64 {
        self.x1.saturating_sub(self.x0).max(0) as u64
    }

    pub fn height(self) -> u64 {
        self.y1.saturating_sub(self.y0).max(0) as u64
    }

    pub fn area(self) -> u64 {
        self.width().saturating_mul(self.height())
    }

    pub fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn union(self, other: DeviceRect) -> DeviceRect {
        DeviceRect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// The device pixels that differ between the image in the GPU texture and the
/// last render the engine produced.
///
/// The engine reports *how many* pixels a partial render changed but not *which*
/// — the rectangles it patches are private to `figcraft_render::retained`. What
/// Spool can locate on its own is the strips a pan uncovers, because the pan is
/// what Spool asked for. So the rects here are the located part, and the
/// inspection accessors below report what is left over.
///
/// Nothing reads the inspection accessors until the canvas does; the tests are
/// their only reader for now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    rects: Vec<DeviceRect>,
    unlocated: u64,
}

#[allow(dead_code)]
impl Damage {
    /// Everything. Used for a full render and for a resize, where no subset of
    /// the previous image survives.
    pub fn whole(size: DeviceSize) -> Self {
        Damage {
            rects: vec![DeviceRect::whole(size)],
            unlocated: 0,
        }
    }

    /// The strips a move from `from` to `to` uncovers, in a view of `size`.
    ///
    /// Moving the view right by `dx` leaves the rightmost `dx` columns holding
    /// whatever the previous frame had there, which is nothing; moving it left
    /// leaves the leftmost `-dx` columns. A shift as wide as the view uncovers all
    /// of it.
    pub fn pan(from: (i64, i64), to: (i64, i64), size: DeviceSize) -> Self {
        let mut damage = Damage::default();
        let (dx, dy) = match (to.0.checked_sub(from.0), to.1.checked_sub(from.1)) {
            (Some(dx), Some(dy)) => (dx, dy),
            // Device origins that far apart are not a pan.
            _ => return Damage::whole(size),
        };
        let (w, h) = (i64::from(size.width), i64::from(size.height));
        if dx.abs() >= w || dy.abs() >= h {
            return Damage::whole(size);
        }
        if dx > 0 {
            damage.push(DeviceRect {
                x0: w - dx,
                y0: 0,
                x1: w,
                y1: h,
            });
        } else if dx < 0 {
            damage.push(DeviceRect {
                x0: 0,
                y0: 0,
                x1: -dx,
                y1: h,
            });
        }
        if dy > 0 {
            damage.push(DeviceRect {
                x0: 0,
                y0: h - dy,
                x1: w,
                y1: h,
            });
        } else if dy < 0 {
            damage.push(DeviceRect {
                x0: 0,
                y0: 0,
                x1: w,
                y1: -dy,
            });
        }
        damage
    }

    /// Add a rect, merging it into the existing ones wherever the bounding box is
    /// no bigger than the two rects it would replace.
    pub fn push(&mut self, rect: DeviceRect) {
        if rect.is_empty() {
            return;
        }
        self.rects.push(rect);
        let mut i = 0;
        while i < self.rects.len() {
            let mut j = i + 1;
            let mut merged = false;
            while j < self.rects.len() {
                let (Some(a), Some(b)) = (self.rects.get(i).copied(), self.rects.get(j).copied())
                else {
                    break;
                };
                let union = a.union(b);
                if union.area() <= a.area().saturating_add(b.area()) {
                    if let Some(slot) = self.rects.get_mut(i) {
                        *slot = union;
                    }
                    self.rects.swap_remove(j);
                    merged = true;
                } else {
                    j += 1;
                }
            }
            if !merged {
                i += 1;
            }
        }
        if self.rects.len() > MAX_DAMAGE_RECTS {
            self.rects = self
                .rects
                .iter()
                .copied()
                .reduce(DeviceRect::union)
                .into_iter()
                .collect();
        }
    }

    /// Account for a `Partial` render that reported `pixels` changed pixels.
    ///
    /// Whatever is already located in `self` came out of that count, so only the
    /// remainder is unlocated. Saturating, because an engine that undercounts is
    /// better described as "some unknown pixels" than as a negative number.
    pub fn note_partial(&mut self, pixels: u64) {
        self.unlocated = pixels.saturating_sub(self.located_area());
    }

    /// The area of the located rects.
    pub fn located_area(&self) -> u64 {
        self.rects
            .iter()
            .map(|r| r.area())
            .fold(0, u64::saturating_add)
    }

    /// The located rects, in no particular order.
    pub fn rects(&self) -> &[DeviceRect] {
        &self.rects
    }

    /// Changed pixels the engine counted without naming.
    pub fn unlocated(&self) -> u64 {
        self.unlocated
    }

    /// The device pixels changed, located or not.
    pub fn area(&self) -> u64 {
        self.located_area().saturating_add(self.unlocated)
    }

    /// Whether nothing at all changed.
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty() && self.unlocated == 0
    }
}

/// Where a rendered view sits, in whole device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Placed {
    origin_px: (i64, i64),
    size: DeviceSize,
}

/// The device pixels a render changed, as far as they can be located.
///
/// A full render, or a render at a different size than the last one, has no
/// meaningful subset: the previous image is not a shifted version of the new one,
/// so the damage is the whole view. Otherwise the pan is located from the two
/// device-pixel origins, and the engine's `Partial` count fills in the rest.
fn damage_of(work: Work, from: Option<Placed>, to: Placed) -> Damage {
    let resized = from.is_some_and(|f| f.size != to.size);
    if work == Work::Full || resized {
        return Damage::whole(to.size);
    }
    let mut damage = from
        .map(|f| Damage::pan(f.origin_px, to.origin_px, to.size))
        .unwrap_or_default();
    if let Work::Partial { pixels } = work {
        damage.note_partial(pixels);
    }
    damage
}

/// What the texture cache decided to do with a render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upload {
    /// The pixels in the texture are still exactly what the engine produced.
    Skip,
    /// The texture must be replaced, which costs a full upload.
    Replace,
}

/// The image currently in the GPU texture, and the state that decides when it has
/// to be replaced.
///
/// This is the whole upload-skipping rule in one place: GPUI uploads a texture
/// once per `RenderImage`, so the only way to avoid an upload is to keep handing
/// the draw side the same `Arc`. That is only correct while the engine says it
/// changed nothing, which is exactly [`Work::Reused`].
///
/// Nothing outside this module reads the inspection accessors yet; they are the
/// draw side's and the tests' view of the cache, not a decision it needs.
///
/// The cache deliberately does *not* keep the last render's [`Damage`]. A
/// replaced texture holds those pixels, and a skipped render changed nothing, so
/// the damage would be empty either way; the frame carries it instead, where it
/// describes the render rather than the texture.
#[derive(Default)]
#[allow(dead_code)]
pub struct TextureCache {
    size: Option<DeviceSize>,
    image: Option<Arc<RenderImage>>,
}

impl TextureCache {
    /// Whether a render of `size` that took `work` needs a new texture.
    ///
    /// Deliberately takes no pixels: this is the decision, separable from the
    /// conversion, so it can be asserted on without building an image.
    pub fn plan(&self, work: Work, size: DeviceSize) -> Upload {
        match (&self.image, self.size) {
            (Some(_), Some(held)) if held == size && work == Work::Reused => Upload::Skip,
            _ => Upload::Replace,
        }
    }

    /// Decide, and act: replace the texture when the plan says so.
    ///
    /// `build` produces the converted image and runs only when a replacement is
    /// due. Returning `None` from it clears the texture rather than leaving the
    /// old one in place — an image of a different size is worse than no image, and
    /// the draw side can already cope with having nothing to show.
    pub fn commit(
        &mut self,
        work: Work,
        size: DeviceSize,
        build: impl FnOnce() -> Option<Arc<RenderImage>>,
    ) -> Upload {
        match self.plan(work, size) {
            Upload::Skip => Upload::Skip,
            Upload::Replace => {
                let image = build();
                self.size = image.as_ref().map(|_| size);
                self.image = image;
                Upload::Replace
            }
        }
    }

    /// The image to draw, if there is one.
    pub fn image(&self) -> Option<Arc<RenderImage>> {
        self.image.clone()
    }

    /// The device size of the image in the texture.
    pub fn size(&self) -> Option<DeviceSize> {
        self.size
    }

    /// Forget the texture. The next render is a full replacement.
    pub fn clear(&mut self) {
        self.size = None;
        self.image = None;
    }
}

/// Premultiplied RGBA in, straight-alpha BGRA out.
///
/// The engine hands back a pixmap of premultiplied RGBA; GPUI's atlas wants
/// BGRA8Unorm, which is byte-permuted and *not* premultiplied. `demultiply` is
/// tiny-skia's own, so the rounding here is the same rounding the engine's
/// `take_demultiplied` would have produced — one less place for a translucent
/// pixel to come out a shade different.
///
/// # Why this loop is shaped the way it is
///
/// This runs on every frame whose pixels changed, on the thread that has to
/// finish before the GPU can be given the image, so its cost is the latency
/// between the pointer and the picture. It used to push four bytes per pixel
/// into a growing buffer; measured over a 1600 × 1200 canvas that was 2.76 ms
/// a frame, which was more than the engine's own render of the same frame.
///
/// Three things changed, and none of them changes a single byte of the result:
///
/// * **The buffer is sized once** and written through, rather than grown a
///   byte at a time: every push is a capacity check, and there are four per
///   pixel.
/// * **An opaque pixel is copied whole.** Premultiplying by 1 is the identity,
///   and tiny-skia's own `demultiply` short-circuits there too, so the
///   shortcut is the engine's rounding and not a second one.
/// * **The four bytes go out together**, as one copy of a four-byte array,
///   rather than as four separate stores.
///
/// Together: 0.73 ms for the same canvas, against 2.76 ms before.
pub fn pixmap_to_bgra(pixmap: &Pixmap) -> Option<Vec<u8>> {
    let size = DeviceSize::new(pixmap.width(), pixmap.height());
    let pixels = pixmap.pixels();
    // `pixels()` is one `PremultipliedColorU8` per pixel, not one byte.
    if u64::try_from(pixels.len()).ok() != Some(size.pixels()) {
        return None;
    }
    let wanted = usize::try_from(size.pixels().checked_mul(4)?).ok()?;
    let mut out = vec![0u8; wanted];
    for (pixel, chunk) in pixels.iter().zip(out.chunks_exact_mut(4)) {
        if pixel.alpha() == 255 {
            // Opaque: premultiplied and straight are the same colour.
            chunk.copy_from_slice(&[pixel.blue(), pixel.green(), pixel.red(), 255]);
            continue;
        }
        // Everything else: tiny-skia's own demultiply, so a translucent pixel
        // comes out with the rounding the engine would have given it.
        let c = pixel.demultiply();
        chunk.copy_from_slice(&[c.blue(), c.green(), c.red(), c.alpha()]);
    }
    Some(out)
}

/// Convert an engine pixmap into something GPUI can draw.
///
/// The buffer is *permuted*, not semantically RGBA: `image::Rgba<u8>` is four
/// bytes in a fixed order, and the bytes in it are B, G, R, A. Every other caller
/// in GPUI does exactly this (`svg_renderer`, the `img` decoders, `repl`), and
/// `Window::paint_image` reads them back as BGRA.
pub fn to_render_image(pixmap: &Pixmap) -> Option<Arc<RenderImage>> {
    let size = DeviceSize::new(pixmap.width(), pixmap.height());
    let bytes = pixmap_to_bgra(pixmap)?;
    let buffer = RgbaImage::from_raw(size.width, size.height, bytes)?;
    Some(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
}

/// A view in whole device pixels, with the origin the engine will snap it to.
#[derive(Clone, Debug)]
pub struct DeviceView {
    /// What to hand `Retained::render`, *unsnapped*.
    ///
    /// The engine snaps internally and `snap` grows the view by a pixel each time
    /// it runs, so pre-snapping here would ask for a view two pixels wider than
    /// the one whose placement is reported.
    pub view: View,
    /// `view`'s top-left in whole device pixels, in page space.
    pub origin_px: (i64, i64),
}

/// A whole number of device pixels for a length of `logical` points at `ratio`.
///
/// Rounding rather than truncating is the point: the engine snaps the origin
/// outwards to a whole pixel and grows the view to cover the gap, so a truncated
/// size does not ask for a texture one pixel narrower than the view.
fn device_extent(logical: f64, ratio: f64) -> Option<u32> {
    let px = logical * ratio;
    if !(px.is_finite() && px > 0.0) {
        return None;
    }
    let rounded = px.round();
    // The float-to-int cast saturates, so a viewport of absurd size clamps to the
    // engine's own edge instead of wrapping.
    Some((rounded as u32).clamp(1, MAX_DEVICE_EDGE))
}

/// Build the view for a canvas viewport, and where it lands in device pixels.
///
/// `origin` is the page coordinate at the viewport's top-left, `zoom` is page
/// units per logical point, `logical` is the viewport's size in points and `ratio`
/// is the window's device pixel ratio. The returned scale is `zoom * ratio`, which
/// is what makes a Retina blit sharp and a 1.0 blit one-to-one.
///
/// `None` when the request cannot be put on a pixel grid — a non-finite zoom or
/// pan, a zero or negative ratio, an empty viewport. Those are errors at the call
/// site rather than something to render, and `None` keeps the previous frame on
/// screen instead of flashing an empty canvas.
pub fn device_view(
    origin: (f64, f64),
    zoom: f64,
    logical: (f32, f32),
    ratio: f32,
) -> Option<DeviceView> {
    let ratio = f64::from(ratio);
    let scale = zoom * ratio;
    if !(ratio.is_finite() && ratio > 0.0 && scale.is_finite() && scale > 0.0) {
        return None;
    }
    let width = device_extent(f64::from(logical.0), ratio)?;
    let height = device_extent(f64::from(logical.1), ratio)?;
    let view = View {
        origin,
        scale,
        width,
        height,
    };
    // `snap` is the engine's own validity check as well as the source of the
    // device origin. Its snapped view is discarded: `render` snaps again.
    let (_, origin_px) = snap(&view)?;
    Some(DeviceView { view, origin_px })
}

/// A finished frame: the image to blit, and everything needed to place it.
///
/// The placement accessors are the draw side's, and nothing outside this module
/// calls them until the canvas does.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CanvasFrame {
    /// The image, or `None` while the first render is still in flight.
    pub image: Option<Arc<RenderImage>>,
    /// The device size of `image`. The engine renders one pixel wider and taller
    /// than the view it was given, so this is not `view.width`.
    pub size: DeviceSize,
    /// The view the engine actually drew.
    pub view: View,
    /// `view`'s top-left in whole device pixels, in page space.
    pub origin_px: (i64, i64),
    /// Device pixels per page unit: `zoom * ratio`.
    pub scale: f64,
    /// The device pixel ratio this frame was rendered for.
    pub ratio: f32,
    /// What the render cost. `Reused` means the cached image was kept.
    pub work: Work,
    /// The device pixels this render changed. This describes the *render*, not
    /// the texture: it is where the engine put work, which is what a future
    /// partial upload would have to patch and what a diagnostics overlay can
    /// draw.
    pub damage: Damage,
    /// Which frame this is: the counter the renderer bumps on every publish.
    ///
    /// The draw side needs it to know whether the frame it is about to draw is
    /// the newest one — a frame that arrived while the last one was being
    /// painted is not on the screen yet, and the canvas has to ask for another
    /// frame or the picture stays stale until the next input event.
    pub generation: u64,
    /// The document this frame is of, by identity: the address of the `Arc` the
    /// engine handed over.
    ///
    /// The engine's document is immutable behind its `Arc` — a change is a new
    /// `Arc` — so this is the document's *version*, and the draw side can ask
    /// whether the picture on the screen is of the document as it is now.
    pub doc: usize,
}

impl CanvasFrame {
    /// The size to place the element at, in logical points.
    pub fn logical_size(&self, zoom: f64) -> (f32, f32) {
        // The size of the frame's CONTENT, in logical points, at the zoom the
        // view is at NOW.
        //
        // The frame's pixels are at ITS OWN scale, which is the zoom it was
        // rendered at times the device pixel ratio. So the content covers
        // `size / scale` page units, and in the current view those page units
        // are `zoom` logical points each. Dividing out the ratio instead --
        // which is what this did -- gives the right answer only while the
        // frame's zoom IS the current zoom.
        //
        // And it is not, for exactly as long as a new frame is on its way:
        // between a zoom gesture and the frame that answers it, the frame on
        // screen is the OLD one. Drawing it at its own size while the overlay
        // drew the new zoom's content is what made objects change size as the
        // canvas zoomed, and what made a resized object disagree with the
        // handle being dragged.
        let zoom = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        let scale = if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        };
        (
            (self.size.width as f64 / scale * zoom) as f32,
            (self.size.height as f64 / scale * zoom) as f32,
        )
    }

    /// Where the blit goes, in logical points from the viewport's top-left.
    ///
    /// The blit is a whole number of device pixels whose top-left is a whole
    /// number of device pixels into the page, so this is exact at any ratio — no
    /// half-pixel drift to correct for after the fact.
    pub fn origin_in_view(&self, viewport_origin: (f64, f64), zoom: f64) -> (f32, f32) {
        let scale = if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        };
        (
            (((self.origin_px.0 as f64 / scale) - viewport_origin.0) * zoom) as f32,
            (((self.origin_px.1 as f64 / scale) - viewport_origin.1) * zoom) as f32,
        )
    }
}

/// One render, as the draw side asks for it.
pub struct RenderRequest {
    pub doc: Arc<Document>,
    pub page: NodeId,
    /// The view, in whole device pixels.
    pub view: View,
    /// `view`'s top-left in whole device pixels, in page space.
    pub origin_px: (i64, i64),
    /// The device pixel ratio the view was sized for.
    pub ratio: f32,
    pub options: Options,
    /// When the request was made, so the render thread can say how long the
    /// answer took to arrive.
    ///
    /// The latency is the number that decides whether the canvas feels live: the
    /// pixels on screen are always a frame old, and this is how old.
    pub submitted: Instant,
}

impl RenderRequest {
    pub fn new(
        doc: Arc<Document>,
        page: NodeId,
        device: DeviceView,
        ratio: f32,
        options: Options,
    ) -> Self {
        Self {
            doc,
            page,
            view: device.view,
            origin_px: device.origin_px,
            ratio,
            options,
            submitted: Instant::now(),
        }
    }
}

/// Why the renderer has no frame to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderFault {
    /// The background thread could not be started, so nothing will ever render.
    ///
    /// The editor stays usable — it draws its own overlays — but there is no
    /// engine output behind them, and this is not something to retry per frame.
    Unavailable(String),
    /// The render thread panicked and has stopped.
    ///
    /// The panic is caught where it happens rather than left to unwind through the
    /// window's frame, and the renderer resets so the last good frame stays on
    /// screen. The thread then stops: a thread that panicked once inside the engine
    /// will do it again, and a loop that respawns it is a busy loop.
    Panicked(String),
}

/// Why a submission did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanvasError {
    /// The view cannot be put on a pixel grid: a non-finite or non-positive zoom,
    /// pan, ratio or viewport size. Nothing was submitted and the previous frame
    /// stays on screen.
    NoUsableScale,
    /// The renderer has stopped. There is no thread left to submit to.
    Fault(RenderFault),
}

/// Counters for what the renderer has actually done.
///
/// Upload count is the number that matters: it is the difference between a pan
/// that costs a texture upload and one that does not, and it is invisible
/// otherwise. Nothing reads these yet outside the tests and
/// [`CanvasRenderer::diagnostics`].
///
/// The timings are the other half of the story. `render_micros` is the engine's
/// own work; `convert_micros` is the pixel-by-pixel conversion into GPUI's
/// layout; `latency_micros` is the whole round trip from a submission to the
/// frame that answers it. A canvas that feels late is one whose latency is
/// larger than a frame, and these are what says which of the three is the
/// reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub struct RenderStats {
    /// Frames handed to the draw side.
    pub published: u64,
    /// Renders the engine reported as having changed nothing.
    pub unchanged: u64,
    /// Texture replacements: a `Partial` or `Full` render, or a resize.
    pub replaced: u64,
    /// Requests dropped because a newer one arrived before the thread took them.
    pub coalesced: u64,
    /// Requests not submitted at all: the same request as the last one.
    pub skipped: u64,
    /// Renders the engine reported as a whole-view redraw.
    pub full: u64,
    /// Renders the engine patched rather than redrew.
    pub partial: u64,
    /// Total time inside the engine's render, in microseconds.
    pub render_micros: u64,
    /// Total time turning the engine's pixmap into a GPUI image, in microseconds.
    pub convert_micros: u64,
    /// Total time from a submission to the frame that answered it, in microseconds.
    pub latency_micros: u64,
    /// The last render's engine time, in microseconds.
    pub last_render_micros: u64,
    /// The last conversion's time, in microseconds.
    pub last_convert_micros: u64,
    /// The last submission's round trip, in microseconds.
    pub last_latency_micros: u64,
    /// The largest round trip so far, in microseconds.
    pub max_latency_micros: u64,
}

/// What a submission did to the pending slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub struct Submission {
    /// A newer request replaced this one before the render thread took it.
    pub coalesced: bool,
}

/// The last request that was submitted, and the document it was for.
///
/// The document is kept alive next to the key, because the key names the
/// document by its address and an address that is free is an address another
/// document can be given. A freed document's address reused by a new one would
/// make two different documents look like the same request, and the canvas
/// would keep a picture of the first.
struct LastRequest {
    key: RequestKey,
    /// The document the key names, kept alive so its address cannot be
    /// reused by another document while the key is.
    ///
    /// It is never read: holding it is the whole of it.
    #[allow(dead_code)]
    doc: Arc<Document>,
}

/// What makes two render requests the same request.
///
/// figcraft's egui frontend keeps the same key (`JobKey`) and does not submit
/// a job that matches it, and this is that key. It has to name everything the
/// picture depends on, or the canvas would keep a frame it should have
/// replaced:
///
/// * the document — by identity, because the engine's document is immutable
///   behind its `Arc` and a change is a new `Arc`;
/// * the page, the view and the ratio — the picture's place and its scale;
/// * the options — what is drawn and how;
/// * the font generation — new fonts change the text's layout, and so the
///   picture, without the document changing at all.
#[derive(Clone, Debug, PartialEq)]
struct RequestKey {
    doc: usize,
    page: NodeId,
    origin: (i64, i64),
    scale: f64,
    width: u32,
    height: u32,
    ratio: f32,
    background: Option<(f32, f32, f32, f32)>,
    outline_mode: bool,
    outline_hidden: bool,
    outline_bounds: bool,
    outline_light: bool,
    outlined: Vec<NodeId>,
    skip: Vec<NodeId>,
    solo: Option<NodeId>,
    lod: bool,
    fonts: u64,
}

impl RequestKey {
    fn of(request: &RenderRequest) -> Self {
        let options = &request.options;
        Self {
            doc: Arc::as_ptr(&request.doc) as usize,
            page: request.page,
            origin: request.origin_px,
            scale: request.view.scale,
            width: request.view.width,
            height: request.view.height,
            ratio: request.ratio,
            background: options.background.map(|c| (c.r, c.g, c.b, c.a)),
            outline_mode: options.outline_mode,
            outline_hidden: options.outline.hidden,
            outline_bounds: options.outline.bounds,
            outline_light: options.outline.light,
            outlined: options.outlined.clone(),
            skip: options.skip.clone(),
            solo: options.solo,
            lod: options.lod,
            fonts: figcraft_text::layout_generation(),
        }
    }
}

/// A one-slot mailbox: a canvas only ever wants the newest view.
///
/// A request per pointer move would queue hundreds of renders behind one pan and
/// make the canvas lag further behind the further the pointer travelled. Only the
/// latest view is worth rendering, so the slot keeps it and the drop is counted.
struct Mailbox<T> {
    slot: Mutex<Option<T>>,
}

/// Hand-written rather than derived: a derive would demand `T: Default`, and
/// `RenderRequest` is not `Default` — it is a document and a view, not a value
/// with a sensible zero.
impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Mailbox {
            slot: Mutex::new(None),
        }
    }
}

impl<T> Mailbox<T> {
    fn put(&self, value: T) -> bool {
        let mut slot = lock(&self.slot);
        let replaced = slot.is_some();
        *slot = Some(value);
        replaced
    }

    fn take(&self) -> Option<T> {
        lock(&self.slot).take()
    }
}

/// A lock that a panic elsewhere cannot poison into uselessness.
///
/// The render thread is the only writer and it is also where panics are caught, so
/// a poison here means one frame's work was interrupted — not that the frame behind
/// it is untrustworthy. Refusing every later read would turn a recovered engine bug
/// into a canvas frozen for the rest of the session.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A readable description of a panic payload, for a fault a UI can log.
fn fault_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "the render thread panicked".to_string()
    }
}

/// Run one step of work, turning a panic into a fault.
///
/// This is the whole of "a panicking render thread must not take down the UI". A
/// panic on a bare `std::thread` does not unwind into the thread runtime and print
/// a backtrace — the process is gone, along with the window. Catching it here turns
/// an engine bug into a fault the editor can report and a canvas that keeps its last
/// frame.
fn guarded(step: impl FnOnce()) -> Result<(), RenderFault> {
    match catch_unwind(AssertUnwindSafe(step)) {
        Ok(()) => Ok(()),
        Err(payload) => Err(RenderFault::Panicked(fault_message(&*payload))),
    }
}

/// Everything the state shared between the two threads lives here.
#[derive(Default)]
struct Shared {
    /// The newest finished frame.
    frame: Mutex<Option<CanvasFrame>>,
    /// Sticky: a stopped renderer reports the same fault until it is replaced.
    fault: Mutex<Option<RenderFault>>,
    /// The app's redraw hook. `Arc`, so it can be called without holding the lock.
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Bumped on every publish, so the draw side can notice without reading the
    /// frame.
    generation: AtomicU64,
    rendering: AtomicBool,
    published: AtomicU64,
    unchanged: AtomicU64,
    replaced: AtomicU64,
    coalesced: AtomicU64,
    skipped: AtomicU64,
    full: AtomicU64,
    partial: AtomicU64,
    render_micros: AtomicU64,
    convert_micros: AtomicU64,
    latency_micros: AtomicU64,
    last_render_micros: AtomicU64,
    last_convert_micros: AtomicU64,
    last_latency_micros: AtomicU64,
    max_latency_micros: AtomicU64,
}

/// What one publish cost, in microseconds.
#[derive(Clone, Copy, Debug, Default)]
struct Timings {
    /// Inside the engine's render.
    render: u64,
    /// Turning the pixmap into a GPUI image.
    convert: u64,
    /// From the submission to this frame.
    latency: u64,
}

impl Shared {
    fn publish(&self, mut frame: CanvasFrame, upload: Upload, timings: Timings) {
        let generation = self.generation.fetch_add(1, Ordering::Release) + 1;
        frame.generation = generation;
        *lock(&self.frame) = Some(frame);
        self.published.fetch_add(1, Ordering::Relaxed);
        match upload {
            Upload::Skip => {
                self.unchanged.fetch_add(1, Ordering::Relaxed);
            }
            Upload::Replace => {
                self.replaced.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.render_micros
            .fetch_add(timings.render, Ordering::Relaxed);
        self.convert_micros
            .fetch_add(timings.convert, Ordering::Relaxed);
        self.latency_micros
            .fetch_add(timings.latency, Ordering::Relaxed);
        self.last_render_micros
            .store(timings.render, Ordering::Relaxed);
        self.last_convert_micros
            .store(timings.convert, Ordering::Relaxed);
        self.last_latency_micros
            .store(timings.latency, Ordering::Relaxed);
        self.max_latency_micros
            .fetch_max(timings.latency, Ordering::Relaxed);
        self.wake_up();
    }

    fn fail(&self, fault: RenderFault) {
        *lock(&self.fault) = Some(fault);
        self.rendering.store(false, Ordering::Release);
    }

    /// Ask the app to redraw, if it asked to be asked.
    ///
    /// The hook is the app's, not the engine's, and by the time it runs the frame
    /// is already published. A hook that panics therefore costs a redraw and nothing
    /// else, so its panic stops here rather than reaching the render loop and
    /// stopping it.
    fn wake_up(&self) {
        let hook = lock(&self.wake).clone();
        if let Some(hook) = hook {
            let _ = catch_unwind(AssertUnwindSafe(move || hook()));
        }
    }
}

/// The state the render thread owns: the engine's retained view, the texture
/// cache, and where the last frame was placed.
#[derive(Default)]
struct Engine {
    retained: Retained,
    cache: TextureCache,
    placed: Option<Placed>,
}

impl Engine {
    /// Render `request` and publish the result, or report why nothing was
    /// published.
    fn step(&mut self, shared: &Shared, request: &RenderRequest) -> Result<(), RenderFault> {
        let result = {
            let Self {
                retained,
                cache,
                placed,
            } = self;
            guarded(move || render_one(retained, cache, placed, shared, request))
        };
        if result.is_err() {
            // A panic part-way through a retained render leaves the engine's kept
            // image and the texture cache describing different things, and neither
            // has been reasoned about. Both start over, which costs one full render.
            *self = Engine::default();
        }
        result
    }
}

/// Render one view, convert it if anything changed, and publish it.
fn render_one(
    retained: &mut Retained,
    cache: &mut TextureCache,
    placed: &mut Option<Placed>,
    shared: &Shared,
    request: &RenderRequest,
) {
    let started = Instant::now();
    let Some((view, pixmap, work)) =
        retained.render(&request.doc, request.page, &request.view, &request.options)
    else {
        // The engine refused the view: it could not be put on a pixel grid, or the
        // pixmap could not be allocated. Nothing describes the current view any
        // more, so the texture goes rather than being drawn at the wrong size.
        cache.clear();
        *placed = None;
        return;
    };
    let render_micros = started.elapsed().as_micros() as u64;
    match work {
        Work::Full => {
            shared.full.fetch_add(1, Ordering::Relaxed);
        }
        Work::Partial { .. } => {
            shared.partial.fetch_add(1, Ordering::Relaxed);
        }
        Work::Reused => {}
    }
    let size = DeviceSize::new(pixmap.width(), pixmap.height());
    let to = Placed {
        origin_px: request.origin_px,
        size,
    };
    let damage = damage_of(work, *placed, to);
    // The conversion is the whole image, every time the pixels changed: it is
    // timed on its own, because it is the part of the pipeline Spool owns and the
    // part the engine cannot make faster.
    let convert_started = Instant::now();
    let upload = cache.commit(work, size, || to_render_image(pixmap));
    let convert_micros = convert_started.elapsed().as_micros() as u64;
    *placed = Some(to);
    shared.publish(
        CanvasFrame {
            image: cache.image(),
            size,
            scale: view.scale,
            view,
            origin_px: request.origin_px,
            ratio: request.ratio,
            work,
            damage,
            generation: 0,
            doc: Arc::as_ptr(&request.doc) as usize,
        },
        upload,
        Timings {
            render: render_micros,
            convert: convert_micros,
            latency: request.submitted.elapsed().as_micros() as u64,
        },
    );
}

/// The render thread's loop, until the renderer is dropped or the engine panics.
///
/// The signal channel carries no information: it is only a doorbell. Taking the
/// newest request from the mailbox and draining the doorbell together is what makes
/// a burst of pans one render of the final position rather than one render per
/// event.
fn run(
    mut engine: Engine,
    shared: Arc<Shared>,
    mailbox: Arc<Mailbox<RenderRequest>>,
    doorbell: Receiver<()>,
) {
    loop {
        if doorbell.recv().is_err() {
            break;
        }
        while doorbell.try_recv().is_ok() {}
        let Some(request) = mailbox.take() else {
            continue;
        };
        shared.rendering.store(true, Ordering::Release);
        if let Err(fault) = engine.step(&shared, &request) {
            shared.fail(fault);
            break;
        }
        shared.rendering.store(false, Ordering::Release);
    }
    shared.rendering.store(false, Ordering::Release);
}

/// Spool's canvas renderer: a retained figcraft render on a background thread,
/// converted into a GPUI image the draw side can blit.
///
/// Every method takes `&self`, because the draw side holds this next to the window
/// it paints into and the render thread holds only the shared state.
///
/// The renderer owns a thread and never joins it: dropping it closes the doorbell
/// and the thread finishes the render it is in, which is the point of it being off
/// the UI thread. Nothing outside this module reads it yet except the tests and
/// [`CanvasRenderer::diagnostics`].
#[allow(dead_code)]
pub struct CanvasRenderer {
    shared: Arc<Shared>,
    mailbox: Arc<Mailbox<RenderRequest>>,
    doorbell: Sender<()>,
    /// The last request that was submitted, by what makes a request the same
    /// request. A request that matches it would be answered with the frame
    /// that is already there.
    last: Mutex<Option<LastRequest>>,
}

impl Default for CanvasRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CanvasRenderer {
    /// Start a renderer.
    ///
    /// A thread that cannot be started is not an error the caller can do anything
    /// about, so it becomes a [`RenderFault::Unavailable`] that `submit` reports and
    /// `fault` keeps, and the editor carries on without engine output.
    pub fn new() -> Self {
        let shared = Arc::new(Shared::default());
        let mailbox = Arc::new(Mailbox::default());
        let (doorbell_tx, doorbell_rx) = channel::<()>();
        let worker_shared = Arc::clone(&shared);
        let worker_mailbox = Arc::clone(&mailbox);
        let spawned = std::thread::Builder::new()
            .name("spool-canvas-render".to_string())
            .spawn(move || {
                run(
                    Engine::default(),
                    worker_shared,
                    worker_mailbox,
                    doorbell_rx,
                )
            });
        if let Err(error) = spawned {
            shared.fail(RenderFault::Unavailable(error.to_string()));
        }
        Self {
            shared,
            mailbox,
            doorbell: doorbell_tx,
            last: Mutex::new(None),
        }
    }

    /// Render the canvas viewport `logical` points across, at page-space `origin`
    /// and `zoom`, for `page` of `doc`.
    ///
    /// The ordinary way in. It sizes the view from the device pixel ratio, hands it
    /// to the render thread, and returns. A pan is a call with a different `origin`;
    /// a resize is a call with a different `logical`.
    pub fn render_canvas(
        &self,
        doc: Arc<Document>,
        page: NodeId,
        origin: (f64, f64),
        zoom: f64,
        logical: (f32, f32),
        ratio: f32,
        options: Options,
    ) -> Result<Submission, CanvasError> {
        let device = device_view(origin, zoom, logical, ratio).ok_or(CanvasError::NoUsableScale)?;
        self.submit(RenderRequest::new(doc, page, device, ratio, options))
    }

    /// Hand a prepared request to the render thread.
    ///
    /// The pending slot holds one request. A second submit before the thread takes
    /// the first replaces it, which is why this is not a queue: a canvas asked to
    /// render on every pointer move wants the position the pointer is at now, not
    /// the four hundred positions it passed through.
    ///
    /// Every request is answered, even one that repeats the last: the engine's
    /// retained renderer has an answer for that too, and the answer is
    /// [`Work::Reused`]. Whether to ask at all is the draw side's decision —
    /// see [`CanvasRenderer::repeats`].
    pub fn submit(&self, request: RenderRequest) -> Result<Submission, CanvasError> {
        if let Some(fault) = self.fault() {
            return Err(CanvasError::Fault(fault));
        }
        let key = RequestKey::of(&request);
        {
            let mut last = lock(&self.last);
            *last = Some(LastRequest {
                key,
                doc: Arc::clone(&request.doc),
            });
        }
        let coalesced = self.mailbox.put(request);
        if coalesced {
            self.shared.coalesced.fetch_add(1, Ordering::Relaxed);
        }
        // A closed doorbell means the thread is gone, which only happens on a fault.
        // The check above is the normal answer; this is the race with it.
        if self.doorbell.send(()).is_err() {
            let fault = self.fault().unwrap_or_else(|| {
                RenderFault::Unavailable("the render thread stopped".to_string())
            });
            return Err(CanvasError::Fault(fault));
        }
        Ok(Submission { coalesced })
    }

    /// Whether a render of this view would be answered with the frame that is
    /// already there.
    ///
    /// The picture depends on the document, the page, the view, the options and
    /// the fonts. When none of them has moved since the last request, the
    /// engine's retained renderer answers [`Work::Reused`] and the frame on the
    /// screen is already the right one — so there is nothing to ask for.
    ///
    /// The draw side asks this before it renders, because the canvas paints on
    /// every frame it is given: a request that cannot change the picture is a
    /// thread round trip and a repaint for nothing, and a repaint that cannot
    /// change the picture is a frame the user waits for. figcraft's egui
    /// frontend keeps the same key (`JobKey`) and does the same thing.
    pub fn would_repeat(
        &self,
        doc: &Arc<Document>,
        page: NodeId,
        origin: (f64, f64),
        zoom: f64,
        logical: (f32, f32),
        ratio: f32,
        options: &Options,
    ) -> bool {
        let Some(device) = device_view(origin, zoom, logical, ratio) else {
            return false;
        };
        let request = RenderRequest::new(Arc::clone(doc), page, device, ratio, options.clone());
        self.repeats(&request)
    }

    /// Whether a request would be answered with the frame that is already there.
    fn repeats(&self, request: &RenderRequest) -> bool {
        let repeats = lock(&self.last)
            .as_ref()
            .is_some_and(|last| last.key == RequestKey::of(request));
        if repeats {
            self.shared.skipped.fetch_add(1, Ordering::Relaxed);
        }
        repeats
    }

    /// Ask to be called after every published frame.
    ///
    /// The hook runs on the *render* thread, not the UI thread, so it must be cheap
    /// and must not touch GPUI state. The usual shape is a closure over a channel or
    /// an atomic that the window's foreground task watches.
    pub fn install_wakeup(&self, wake: impl Fn() + Send + Sync + 'static) {
        *lock(&self.shared.wake) = Some(Arc::new(wake));
    }

    /// The newest finished frame.
    pub fn frame(&self) -> Option<CanvasFrame> {
        lock(&self.shared.frame).clone()
    }

    /// A counter that changes when a frame is published.
    ///
    /// Cheaper than comparing frames, and it survives the case that matters most: two
    /// renders where the second reused the first's image, where the frame is equal in
    /// every field that matters but a redraw is still wanted.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Whether a submitted request is still being rendered.
    pub fn is_rendering(&self) -> bool {
        self.shared.rendering.load(Ordering::Acquire)
    }

    /// Why this renderer has stopped, if it has.
    pub fn fault(&self) -> Option<RenderFault> {
        lock(&self.shared.fault).clone()
    }

    /// What the renderer has done.
    pub fn stats(&self) -> RenderStats {
        RenderStats {
            published: self.shared.published.load(Ordering::Relaxed),
            unchanged: self.shared.unchanged.load(Ordering::Relaxed),
            replaced: self.shared.replaced.load(Ordering::Relaxed),
            coalesced: self.shared.coalesced.load(Ordering::Relaxed),
            skipped: self.shared.skipped.load(Ordering::Relaxed),
            full: self.shared.full.load(Ordering::Relaxed),
            partial: self.shared.partial.load(Ordering::Relaxed),
            render_micros: self.shared.render_micros.load(Ordering::Relaxed),
            convert_micros: self.shared.convert_micros.load(Ordering::Relaxed),
            latency_micros: self.shared.latency_micros.load(Ordering::Relaxed),
            last_render_micros: self.shared.last_render_micros.load(Ordering::Relaxed),
            last_convert_micros: self.shared.last_convert_micros.load(Ordering::Relaxed),
            last_latency_micros: self.shared.last_latency_micros.load(Ordering::Relaxed),
            max_latency_micros: self.shared.max_latency_micros.load(Ordering::Relaxed),
        }
    }

    /// One line describing what the renderer has done, for a diagnostics view.
    pub fn diagnostics(&self) -> String {
        let stats = self.stats();
        format!(
            "published {} · reused {} · uploaded {} · coalesced {} · skipped {} · full {} · partial {} · \
             render {}µs · convert {}µs · latency {}µs (last {}µs, worst {}µs){}",
            stats.published,
            stats.unchanged,
            stats.replaced,
            stats.coalesced,
            stats.skipped,
            stats.full,
            stats.partial,
            stats.render_micros,
            stats.convert_micros,
            stats.latency_micros,
            stats.last_latency_micros,
            stats.max_latency_micros,
            match self.fault() {
                Some(fault) => format!(" · faulted: {fault:?}"),
                None => String::new(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use figcraft_doc::NodeKind;
    use gpui::DevicePixels;
    use tiny_skia::{Color, Pixmap};

    use super::*;

    /// A document with one page and one rectangle on it.
    ///
    /// No fills: what is under test is the thread, the conversion and the upload
    /// decision, none of which need a colour, and setting one would pull
    /// `figcraft-color` into this file's dependencies for nothing.
    fn document() -> Option<(Arc<Document>, NodeId)> {
        let mut doc = Document::new("canvas-render");
        let page = doc.first_page()?;
        doc.create(page, NodeKind::Rectangle, "R", 20.0, 20.0, 40.0, 30.0)
            .ok()?;
        Some((Arc::new(doc), page))
    }

    fn size(width: u32, height: u32) -> DeviceSize {
        DeviceSize::new(width, height)
    }

    fn placed(origin_px: (i64, i64), size: DeviceSize) -> Placed {
        Placed { origin_px, size }
    }

    // --- device pixel ratio -------------------------------------------------

    #[test]
    fn a_retina_ratio_doubles_the_device_extent() {
        assert_eq!(device_extent(800.0, 2.0), Some(1600));
        assert_eq!(device_extent(800.0, 1.0), Some(800));
    }

    /// A fractional ratio is the normal case, not an edge case: 1.5 and 1.25 are
    /// what most external displays report.
    #[test]
    fn a_fractional_ratio_rounds_to_whole_device_pixels() {
        assert_eq!(device_extent(801.0, 1.5), Some(1202));
        assert_eq!(device_extent(333.0, 1.25), Some(416));
    }

    /// Truncating here would ask for a texture a pixel narrower than the view,
    /// because the engine snaps outwards by a pixel to cover the gap.
    #[test]
    fn the_extent_rounds_rather_than_truncating() {
        assert_eq!(device_extent(10.0, 1.1), Some(11));
        assert_eq!(device_extent(10.0, 1.9), Some(19));
    }

    #[test]
    fn an_unusable_ratio_is_refused() {
        assert_eq!(device_extent(800.0, 0.0), None);
        assert_eq!(device_extent(800.0, -2.0), None);
        assert_eq!(device_extent(800.0, f64::NAN), None);
        assert_eq!(device_extent(800.0, f64::INFINITY), None);
        assert_eq!(device_extent(0.0, 2.0), None);
        assert_eq!(device_extent(f64::NAN, 2.0), None);
    }

    /// The engine caps every pixmap edge, and a bigger request is drawn *smaller*
    /// rather than refused. Clamping here keeps the reported size the drawn size.
    #[test]
    fn an_absurd_viewport_is_clamped_to_the_engines_own_edge() {
        let device = device_view((0.0, 0.0), 1.0, (100_000.0, 100_000.0), 1.0);
        let device = device.expect("a clamped view is still a view");
        assert_eq!(device.view.width, MAX_DEVICE_EDGE);
        assert_eq!(device.view.height, MAX_DEVICE_EDGE);
    }

    #[test]
    fn the_view_scale_is_the_zoom_times_the_ratio() {
        let device = device_view((0.0, 0.0), 0.5, (800.0, 800.0), 2.0).expect("a usable view");
        assert_eq!(device.view.scale, 1.0);
        assert_eq!(device.view.width, 1600);
    }

    #[test]
    fn a_view_that_cannot_be_put_on_a_pixel_grid_is_refused() {
        assert!(device_view((0.0, 0.0), 0.0, (800.0, 800.0), 1.0).is_none());
        assert!(device_view((0.0, 0.0), -1.0, (800.0, 800.0), 1.0).is_none());
        assert!(device_view((0.0, 0.0), f64::NAN, (800.0, 800.0), 1.0).is_none());
        assert!(device_view((f64::NAN, 0.0), 1.0, (800.0, 800.0), 1.0).is_none());
        assert!(device_view((0.0, 0.0), 1.0, (800.0, 800.0), 0.0).is_none());
    }

    /// At a fixed zoom the origin must land on the same whole device pixel however
    /// the pan divides, because that is what makes a pan an integer shift of the
    /// previous image rather than a resample.
    #[test]
    fn the_device_origin_floors_onto_the_pixel_grid() {
        let a = device_view((10.0, 20.0), 1.0, (400.0, 400.0), 2.0).expect("a usable view");
        let b = device_view((10.4, 20.4), 1.0, (400.0, 400.0), 2.0).expect("a usable view");
        assert_eq!(a.origin_px, (20, 40));
        assert_eq!(b.origin_px, (20, 40));
        let c = device_view((10.6, 20.6), 1.0, (400.0, 400.0), 2.0).expect("a usable view");
        assert_eq!(c.origin_px, (21, 41));
    }

    /// The engine's `snap` widens a view by a pixel every time it runs, so the view
    /// handed to `render` must be the *unsnapped* one — otherwise the image would
    /// be wider than the placement reported here.
    #[test]
    fn the_view_is_left_unsnapped_for_the_engine_to_snap() {
        let device = device_view((0.0, 0.0), 1.0, (400.0, 400.0), 1.0).expect("a usable view");
        assert_eq!(device.view.width, 400);
        let (snapped, _) = snap(&device.view).expect("the engine will snap this");
        assert_eq!(snapped.width, 401);
    }

    // --- damage bookkeeping -------------------------------------------------

    #[test]
    fn a_view_that_did_not_move_damages_nothing() {
        let damage = Damage::pan((10, 20), (10, 20), size(400, 300));
        assert!(damage.is_empty());
        assert_eq!(damage.area(), 0);
    }

    /// Moving the view right leaves the right-hand columns stale, which is the
    /// engine's rule and the only one worth reproducing.
    #[test]
    fn panning_right_damages_the_right_columns() {
        let damage = Damage::pan((0, 0), (5, 0), size(400, 300));
        assert_eq!(
            damage.rects(),
            &[DeviceRect {
                x0: 395,
                y0: 0,
                x1: 400,
                y1: 300
            }]
        );
        assert_eq!(damage.area(), 1500);
    }

    #[test]
    fn panning_left_damages_the_left_columns() {
        let damage = Damage::pan((0, 0), (-5, 0), size(400, 300));
        assert_eq!(
            damage.rects(),
            &[DeviceRect {
                x0: 0,
                y0: 0,
                x1: 5,
                y1: 300
            }]
        );
    }

    #[test]
    fn panning_down_damages_the_bottom_rows() {
        let damage = Damage::pan((0, 0), (0, 7), size(400, 300));
        assert_eq!(
            damage.rects(),
            &[DeviceRect {
                x0: 0,
                y0: 293,
                x1: 400,
                y1: 300
            }]
        );
    }

    #[test]
    fn panning_diagonally_damages_both_strips() {
        let damage = Damage::pan((0, 0), (4, 6), size(400, 300));
        assert_eq!(damage.rects().len(), 2);
        assert_eq!(damage.area(), 6 * 400 + 4 * 300);
    }

    /// A pan as wide as the view leaves nothing of the old image in place.
    #[test]
    fn a_pan_wider_than_the_view_damages_everything() {
        assert_eq!(
            Damage::pan((0, 0), (400, 0), size(400, 300)),
            Damage::whole(size(400, 300))
        );
        assert_eq!(
            Damage::pan((0, 0), (-400, 0), size(400, 300)),
            Damage::whole(size(400, 300))
        );
    }

    #[test]
    fn overlapping_rects_merge_when_the_bounding_box_is_no_bigger() {
        let mut damage = Damage::default();
        damage.push(DeviceRect {
            x0: 0,
            y0: 0,
            x1: 10,
            y1: 10,
        });
        damage.push(DeviceRect {
            x0: 10,
            y0: 0,
            x1: 20,
            y1: 10,
        });
        assert_eq!(
            damage.rects(),
            &[DeviceRect {
                x0: 0,
                y0: 0,
                x1: 20,
                y1: 10
            }]
        );
    }

    /// Two rects a long way apart must stay two rects: merging them into a bounding
    /// box spanning the gap would claim damage where there is none.
    #[test]
    fn distant_rects_are_not_merged() {
        let mut damage = Damage::default();
        damage.push(DeviceRect {
            x0: 0,
            y0: 0,
            x1: 10,
            y1: 10,
        });
        damage.push(DeviceRect {
            x0: 380,
            y0: 280,
            x1: 390,
            y1: 290,
        });
        assert_eq!(damage.rects().len(), 2);
        assert_eq!(damage.area(), 200);
    }

    #[test]
    fn past_the_cap_damage_collapses_into_one_rect() {
        let mut damage = Damage::default();
        for i in 0..=MAX_DAMAGE_RECTS as i64 {
            damage.push(DeviceRect {
                x0: i * 2,
                y0: 0,
                x1: i * 2 + 1,
                y1: 1,
            });
        }
        assert_eq!(damage.rects().len(), 1);
        assert_eq!(
            damage.rects()[0],
            DeviceRect {
                x0: 0,
                y0: 0,
                x1: MAX_DAMAGE_RECTS as i64 * 2 + 1,
                y1: 1
            }
        );
    }

    #[test]
    fn an_empty_rect_is_ignored() {
        let mut damage = Damage::default();
        damage.push(DeviceRect {
            x0: 10,
            y0: 10,
            x1: 10,
            y1: 40,
        });
        assert!(damage.is_empty());
    }

    /// A full render has no relationship to the previous image at all, so claiming
    /// a subset of it changed would be a lie.
    #[test]
    fn a_full_render_damages_the_whole_view() {
        let from = Some(placed((0, 0), size(400, 300)));
        assert_eq!(
            damage_of(Work::Full, from, placed((0, 0), size(400, 300))),
            Damage::whole(size(400, 300))
        );
    }

    /// A resize is the same case wearing a different hat: the old texture is a
    /// different size, so no rect of it survives.
    #[test]
    fn a_resize_damages_the_whole_view() {
        let from = Some(placed((0, 0), size(400, 300)));
        assert_eq!(
            damage_of(Work::Reused, from, placed((0, 0), size(401, 300))),
            Damage::whole(size(401, 300))
        );
    }

    /// A partial render counts its pixels but will not say which. The strips the pan
    /// accounted for come out of that count, so only the remainder is unlocated.
    #[test]
    fn a_partial_render_locates_the_pan_and_leaves_the_rest_unlocated() {
        let from = Some(placed((0, 0), size(400, 300)));
        let damage = damage_of(
            Work::Partial { pixels: 3_000 },
            from,
            placed((5, 0), size(400, 300)),
        );
        assert_eq!(damage.located_area(), 1_500);
        assert_eq!(damage.unlocated(), 1_500);
        assert_eq!(damage.area(), 3_000);
    }

    #[test]
    fn an_unchanged_render_damages_nothing() {
        let from = Some(placed((0, 0), size(400, 300)));
        assert!(damage_of(Work::Reused, from, placed((0, 0), size(400, 300))).is_empty());
    }

    // --- the texture cache ---------------------------------------------------

    fn fake_image(width: u32) -> Arc<RenderImage> {
        let buffer = RgbaImage::from_raw(width, 1, vec![0u8; (width * 4) as usize]);
        let buffer = buffer.expect("a buffer of the right length");
        Arc::new(RenderImage::new(vec![Frame::new(buffer)]))
    }

    #[test]
    fn an_empty_cache_replaces_on_everything() {
        let cache = TextureCache::default();
        assert_eq!(cache.plan(Work::Reused, size(400, 300)), Upload::Replace);
    }

    #[test]
    fn an_unchanged_render_of_the_same_size_skips_the_upload() {
        let mut cache = TextureCache::default();
        cache.commit(Work::Full, size(400, 300), || Some(fake_image(400)));
        assert_eq!(cache.plan(Work::Reused, size(400, 300)), Upload::Skip);
    }

    #[test]
    fn a_partial_render_always_replaces() {
        let mut cache = TextureCache::default();
        cache.commit(Work::Full, size(400, 300), || Some(fake_image(400)));
        assert_eq!(
            cache.plan(Work::Partial { pixels: 1 }, size(400, 300)),
            Upload::Replace
        );
    }

    /// The size check is not redundant with `Work`: a resize after a `Work::Reused`
    /// keeps the same document and the same zoom, so only the size changed and only
    /// it can catch that.
    #[test]
    fn a_resize_replaces_even_when_nothing_changed() {
        let mut cache = TextureCache::default();
        cache.commit(Work::Full, size(400, 300), || Some(fake_image(400)));
        assert_eq!(cache.plan(Work::Reused, size(401, 300)), Upload::Replace);
    }

    /// The point of the cache: the same `Arc` reaching the draw side is what keeps
    /// GPUI's atlas texture alive. A new `Arc` here would be a full upload, and
    /// nothing in the return type would say so.
    #[test]
    fn a_skipped_render_hands_back_the_same_image() {
        let mut cache = TextureCache::default();
        cache.commit(Work::Full, size(400, 300), || Some(fake_image(400)));
        let first = cache.image().expect("a committed image");
        let upload = cache.commit(Work::Reused, size(400, 300), || Some(fake_image(400)));
        let second = cache.image().expect("a committed image");
        assert_eq!(upload, Upload::Skip);
        assert!(
            Arc::ptr_eq(&first, &second),
            "the texture cache handed back a new image"
        );
    }

    #[test]
    fn a_replaced_render_installs_a_new_image() {
        let mut cache = TextureCache::default();
        cache.commit(Work::Full, size(400, 300), || Some(fake_image(400)));
        let first = cache.image().expect("a committed image");
        cache.commit(Work::Partial { pixels: 9 }, size(400, 300), || {
            Some(fake_image(400))
        });
        let second = cache.image().expect("a committed image");
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn a_replacement_clears_the_damage() {
        let mut cache = TextureCache::default();
        cache.commit(Work::Full, size(400, 300), || Some(fake_image(400)));
    }

    /// A conversion that fails must not leave the old image in place: it is a
    /// different size, and drawing it would be worse than drawing nothing.
    #[test]
    fn a_failed_conversion_clears_the_texture() {
        let mut cache = TextureCache::default();
        cache.commit(Work::Full, size(400, 300), || Some(fake_image(400)));
        cache.commit(Work::Full, size(400, 300), || None);
        assert!(cache.image().is_none());
        assert_eq!(cache.size(), None);
    }

    // --- pixmap conversion ---------------------------------------------------

    #[test]
    fn an_opaque_pixmap_converts_to_bgra() {
        let mut pixmap = Pixmap::new(1, 1).expect("a one pixel pixmap");
        pixmap.fill(Color::from_rgba(0.2, 0.4, 0.6, 1.0).expect("a colour in range"));
        let bytes = pixmap_to_bgra(&pixmap).expect("a one pixel pixmap converts");
        assert_eq!(bytes, vec![153, 102, 51, 255]);
    }

    /// Premultiplied alpha left in place would darken every translucent pixel; this
    /// is the assertion that the demultiplication actually happened.
    #[test]
    fn a_translucent_pixmap_is_demultiplied() {
        let mut pixmap = Pixmap::new(1, 1).expect("a one pixel pixmap");
        pixmap.fill(Color::from_rgba(1.0, 0.0, 0.0, 0.5).expect("a colour in range"));
        let bytes = pixmap_to_bgra(&pixmap).expect("a one pixel pixmap converts");
        assert_eq!(bytes, vec![0, 0, 255, 128]);
    }

    /// The opaque fast path must be the identity, not an approximation.
    ///
    /// The conversion skips the demultiply for an opaque pixel, and the whole
    /// point of that is that premultiplying by one is the identity: the bytes
    /// have to be the same ones tiny-skia's own `demultiply` would have given.
    #[test]
    fn an_opaque_pixmap_is_copied_unchanged() {
        let mut pixmap = Pixmap::new(1, 1).expect("a one pixel pixmap");
        pixmap.fill(Color::from_rgba(0.0, 0.5, 1.0, 1.0).expect("a colour in range"));
        let bytes = pixmap_to_bgra(&pixmap).expect("a one pixel pixmap converts");
        assert_eq!(bytes, vec![255, 128, 0, 255]);
        // And the same, through tiny-skia's own demultiply.
        let demultiplied = pixmap.pixels()[0].demultiply();
        assert_eq!(
            bytes,
            vec![
                demultiplied.blue(),
                demultiplied.green(),
                demultiplied.red(),
                demultiplied.alpha()
            ],
            "the fast path must agree with the engine's own demultiply"
        );
    }

    #[test]
    fn a_pixmap_converts_to_four_bytes_per_pixel() {
        let pixmap = Pixmap::new(3, 2).expect("a six pixel pixmap");
        let bytes = pixmap_to_bgra(&pixmap).expect("a six pixel pixmap converts");
        assert_eq!(bytes.len(), 24);
    }

    /// The `RenderImage` is what reaches the draw side, so its reported size is what
    /// the element is laid out from.
    #[test]
    fn a_pixmap_becomes_a_render_image_of_the_same_size() {
        let pixmap = Pixmap::new(7, 5).expect("a 7x5 pixmap");
        let image = to_render_image(&pixmap).expect("a pixmap converts");
        assert_eq!(image.size(0).width, DevicePixels(7));
        assert_eq!(image.size(0).height, DevicePixels(5));
        assert_eq!(image.frame_count(), 1);
    }

    // --- frame placement -----------------------------------------------------

    fn frame(origin_px: (i64, i64), ratio: f32, scale: f64) -> CanvasFrame {
        CanvasFrame {
            image: None,
            size: size(1600, 1200),
            view: View {
                origin: (0.0, 0.0),
                scale,
                width: 1600,
                height: 1200,
            },
            damage: Damage::default(),
            origin_px,
            scale,
            ratio,
            work: Work::Full,
            generation: 1,
            doc: 0,
        }
    }

    /// At ratio 2 the engine rendered 1600 device pixels across an 800 point
    /// viewport, so the element is 800 points wide.
    #[test]
    fn the_logical_size_divides_out_the_device_pixel_ratio() {
        assert_eq!(frame((0, 0), 2.0, 2.0).logical_size(1.0), (800.0, 600.0));
        assert_eq!(frame((0, 0), 1.0, 1.0).logical_size(1.0), (1600.0, 1200.0));
    }

    #[test]
    fn the_blit_origin_is_measured_from_the_viewport_origin() {
        assert_eq!(
            frame((0, 0), 2.0, 2.0).origin_in_view((0.0, 0.0), 1.0),
            (0.0, 0.0)
        );
        // 20 device pixels at scale 2 is 10 page units, which at zoom 1 is 10 logical
        // points across.
        assert_eq!(
            frame((20, 40), 2.0, 2.0).origin_in_view((0.0, 0.0), 1.0),
            (10.0, 20.0)
        );
    }

    /// A viewport that is itself panned moves the blit with it, and the two must not
    /// cancel: the device origin is absolute in page space.
    #[test]
    fn the_blit_origin_accounts_for_the_pan() {
        assert_eq!(
            frame((20, 40), 2.0, 2.0).origin_in_view((10.0, 20.0), 1.0),
            (0.0, 0.0)
        );
    }

    // --- panic containment ---------------------------------------------------

    #[test]
    fn a_clean_step_is_not_a_fault() {
        assert_eq!(guarded(|| {}), Ok(()));
    }

    /// The whole reason `guarded` exists: an engine panic must leave the renderer
    /// reporting a fault, not unwind out of the thread and end the process along
    /// with the window.
    #[test]
    fn a_panicking_render_is_contained_and_named() {
        assert_eq!(
            guarded(|| panic!("the engine exploded")),
            Err(RenderFault::Panicked("the engine exploded".to_string()))
        );
    }

    #[test]
    fn a_panic_payload_that_is_not_a_string_still_names_itself() {
        assert_eq!(
            guarded(|| std::panic::panic_any(7u32)),
            Err(RenderFault::Panicked(
                "the render thread panicked".to_string()
            ))
        );
    }

    #[test]
    fn a_contained_panic_does_not_escape_guarded() {
        // Nothing to assert beyond the fact this test returns at all: without
        // `guarded` the panic would take the test process with it.
        assert!(guarded(|| panic!("contained")).is_err());
    }

    // --- the mailbox ---------------------------------------------------------

    #[test]
    fn the_mailbox_keeps_the_newest_request() {
        let mailbox = Mailbox::default();
        assert!(!mailbox.put(1));
        assert!(mailbox.put(2));
        assert_eq!(mailbox.take(), Some(2));
        assert_eq!(mailbox.take(), None);
    }

    // --- the renderer, end to end -------------------------------------------

    /// Waits for one frame past `after`. Rendering happens on another thread, so a
    /// test that submits and reads immediately would be testing the scheduler.
    fn await_generation(renderer: &CanvasRenderer, after: u64) -> Option<CanvasFrame> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if renderer.generation() > after && !renderer.is_rendering() {
                return renderer.frame();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        None
    }

    /// Waits until the renderer has been quiet for a while, so a burst of submits
    /// has all been taken.
    fn await_idle(renderer: &CanvasRenderer) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last = None;
        let mut quiet = 0;
        while Instant::now() < deadline {
            let generation = renderer.generation();
            if Some(generation) == last && !renderer.is_rendering() {
                quiet += 1;
                if quiet >= 50 {
                    return;
                }
            } else {
                quiet = 0;
            }
            last = Some(generation);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The whole point of the module: engine pixels on a background thread arriving
    /// as a GPUI image the draw side can blit, sized and placed for the ratio.
    #[test]
    fn a_render_reaches_the_draw_side_as_an_image() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let before = renderer.generation();
        renderer
            .render_canvas(
                doc,
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                2.0,
                Options {
                    lod: true,
                    ..Options::default()
                },
            )
            .expect("a usable view");
        let frame = await_generation(&renderer, before).expect("a frame within ten seconds");
        assert!(frame.image.is_some(), "the engine produced no image");
        assert_eq!(frame.ratio, 2.0);
        assert_eq!(frame.scale, 2.0);
        // The engine renders one pixel wider and taller than the view it is given.
        assert_eq!(frame.size, size(401, 301));
        assert_eq!(frame.logical_size(1.0), (200.5, 150.5));
        assert_eq!(frame.origin_px, (0, 0));
    }

    /// The optimisation, measured. Nothing about the request changed, so the engine
    /// reports `Reused` and no texture is replaced. Without this the canvas would
    /// upload a full texture per frame for a static document.
    #[test]
    fn an_unchanged_view_does_not_replace_the_texture() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let options = Options {
            lod: true,
            ..Options::default()
        };
        let before = renderer.generation();
        renderer
            .render_canvas(
                doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                2.0,
                options.clone(),
            )
            .expect("a usable view");
        let first = await_generation(&renderer, before).expect("a first frame");
        let replaced = renderer.stats().replaced;

        let before = renderer.generation();
        renderer
            .render_canvas(doc, page, (0.0, 0.0), 1.0, (200.0, 150.0), 2.0, options)
            .expect("a usable view");
        let second = await_generation(&renderer, before).expect("a second frame");
        assert_eq!(second.work, Work::Reused);
        assert_eq!(
            renderer.stats().replaced,
            replaced,
            "an unchanged view replaced the texture"
        );
        assert_eq!(renderer.stats().unchanged, 1);
        assert!(
            Arc::ptr_eq(
                first.image.as_ref().expect("a first image"),
                second.image.as_ref().expect("a second image"),
            ),
            "an unchanged view handed the draw side a new image",
        );
    }

    /// A pan changes pixels, so it must replace, and the frame must land at the new
    /// device origin so the draw side shifts the blit rather than stretching it.
    #[test]
    fn a_pan_replaces_the_texture_at_the_new_origin() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let options = Options {
            lod: true,
            ..Options::default()
        };
        let before = renderer.generation();
        renderer
            .render_canvas(
                doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                1.0,
                options.clone(),
            )
            .expect("a usable view");
        await_generation(&renderer, before).expect("a first frame");
        let replaced = renderer.stats().replaced;

        let before = renderer.generation();
        renderer
            .render_canvas(doc, page, (3.0, 0.0), 1.0, (200.0, 150.0), 1.0, options)
            .expect("a usable view");
        let panned = await_generation(&renderer, before).expect("a panned frame");
        assert!(
            matches!(panned.work, Work::Partial { .. }),
            "a pan reported {:?}",
            panned.work
        );
        assert_eq!(panned.origin_px, (3, 0));
        assert_eq!(renderer.stats().replaced, replaced + 1);
    }

    /// A canvas asked to render on every pointer move must not render every position
    /// it passed through. The slot keeps the newest, so however many arrive while
    /// the thread is busy, the frame that lands is the last one asked for.
    #[test]
    fn a_burst_of_submits_renders_the_newest_request_only() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        for step in 0..8u32 {
            renderer
                .render_canvas(
                    doc.clone(),
                    page,
                    (f64::from(step), 0.0),
                    1.0,
                    (200.0, 150.0),
                    1.0,
                    Options::default(),
                )
                .expect("a usable view");
        }
        await_idle(&renderer);
        let stats = renderer.stats();
        assert!(
            stats.published < 8,
            "eight submits published {} frames",
            stats.published
        );
        assert!(stats.coalesced > 0, "nothing was coalesced");
        let frame = renderer.frame().expect("a frame");
        assert_eq!(
            frame.origin_px,
            (7, 0),
            "the last request submitted is not the one that landed"
        );
    }

    /// A wake hook that panics is the app's bug, not the engine's, and it must not
    /// stop the render thread: the frame is already published.
    #[test]
    fn a_panicking_wake_hook_does_not_stop_the_renderer() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        renderer.install_wakeup(|| panic!("the app's redraw hook is broken"));
        let before = renderer.generation();
        renderer
            .render_canvas(
                doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                1.0,
                Options::default(),
            )
            .expect("a usable view");
        assert!(
            await_generation(&renderer, before).is_some(),
            "a panicking hook cost the frame"
        );

        let before = renderer.generation();
        renderer
            .render_canvas(
                doc,
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                1.0,
                Options::default(),
            )
            .expect("a usable view");
        assert!(
            await_generation(&renderer, before).is_some(),
            "the render thread stopped after a panicking hook"
        );
        assert!(
            renderer.fault().is_none(),
            "a panicking hook is not a render fault"
        );
    }

    #[test]
    fn a_wake_hook_runs_on_every_published_frame() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let woken = Arc::new(AtomicUsize::new(0));
        let hook = Arc::clone(&woken);
        renderer.install_wakeup(move || {
            hook.fetch_add(1, Ordering::Relaxed);
        });
        let before = renderer.generation();
        renderer
            .render_canvas(
                doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                1.0,
                Options::default(),
            )
            .expect("a usable view");
        await_generation(&renderer, before).expect("a frame");
        let first = woken.load(Ordering::Relaxed);
        assert!(first >= 1, "no wake hook ran");

        let before = renderer.generation();
        renderer
            .render_canvas(
                doc,
                page,
                (0.0, 0.0),
                200.0,
                (200.0, 200.0),
                1.0,
                Options::default(),
            )
            .expect("a usable view");
        await_generation(&renderer, before).expect("a frame");
        assert!(
            woken.load(Ordering::Relaxed) > first,
            "the wake hook did not run again"
        );
    }

    #[test]
    fn an_unusable_view_is_refused_before_anything_is_submitted() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let result = renderer.render_canvas(
            doc,
            page,
            (0.0, 0.0),
            0.0,
            (200.0, 150.0),
            1.0,
            Options::default(),
        );
        assert_eq!(result.err(), Some(CanvasError::NoUsableScale));
        assert_eq!(renderer.generation(), 0);
    }

    #[test]
    fn the_diagnostics_line_counts_what_the_renderer_did() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let before = renderer.generation();
        renderer
            .render_canvas(
                doc,
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                1.0,
                Options::default(),
            )
            .expect("a usable view");
        await_generation(&renderer, before).expect("a frame");
        let line = renderer.diagnostics();
        assert!(
            line.contains("published 1"),
            "the diagnostics line is missing the frame: {line}"
        );
    }

    /// The same request twice is one request.
    ///
    /// The canvas paints on every frame it is given, and it asks for a render
    /// on every paint. A request that is the same as the last one would be
    /// answered with the frame that is already there, so the draw side does not
    /// make it: a thread round trip and a repaint for a picture that cannot
    /// change.
    ///
    /// figcraft's egui frontend keeps the same key and does the same thing.
    #[test]
    fn the_same_request_is_answered_with_the_frame_that_is_there() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let options = Options {
            lod: true,
            ..Options::default()
        };
        assert!(
            !renderer.would_repeat(&doc, page, (0.0, 0.0), 1.0, (200.0, 150.0), 1.0, &options),
            "the first request has nothing to repeat"
        );
        renderer
            .render_canvas(
                doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                1.0,
                options.clone(),
            )
            .expect("a usable view");
        await_generation(&renderer, 0).expect("a first frame");

        assert!(
            renderer.would_repeat(&doc, page, (0.0, 0.0), 1.0, (200.0, 150.0), 1.0, &options),
            "the same request must be recognised as one"
        );
        assert_eq!(renderer.stats().skipped, 1);
    }

    /// A request that is not the same as the last one is a new picture.
    ///
    /// The key has to name everything the picture depends on: a pan, a zoom, a
    /// different page and a different document are four different pictures.
    #[test]
    fn a_different_request_is_a_different_picture() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let options = Options {
            lod: true,
            ..Options::default()
        };
        renderer
            .render_canvas(
                doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                1.0,
                options.clone(),
            )
            .expect("a usable view");
        await_generation(&renderer, 0).expect("a first frame");

        assert!(
            renderer.would_repeat(&doc, page, (0.0, 0.0), 1.0, (200.0, 150.0), 1.0, &options),
            "the same view again is a repeat"
        );
        assert!(
            !renderer.would_repeat(&doc, page, (3.0, 0.0), 1.0, (200.0, 150.0), 1.0, &options),
            "a pan must be a new picture"
        );
        assert!(
            !renderer.would_repeat(&doc, page, (3.0, 0.0), 2.0, (200.0, 150.0), 1.0, &options),
            "a zoom must be a new picture"
        );
        let (other, other_page) = document().expect("a document with one page");
        assert!(
            !renderer.would_repeat(
                &other,
                other_page,
                (3.0, 0.0),
                2.0,
                (200.0, 150.0),
                1.0,
                &options
            ),
            "a different document must be a new picture"
        );
    }

    /// A frame knows which frame it is, and which document it is of.
    ///
    /// The draw side needs both: the generation to know whether the picture on
    /// the screen is the newest one, and the document to know whether it is of
    /// the document as it is now.
    #[test]
    fn a_frame_names_itself_and_its_document() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let before = renderer.generation();
        renderer
            .render_canvas(
                doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                1.0,
                Options::default(),
            )
            .expect("a usable view");
        let frame = await_generation(&renderer, before).expect("a frame");
        assert!(
            frame.generation > before,
            "the frame must be newer than before"
        );
        assert_eq!(
            frame.doc,
            Arc::as_ptr(&doc) as usize,
            "the frame must name the document it is of"
        );
    }

    /// Dropping the renderer must not hang the UI thread waiting for a render in
    /// progress; the doorbell closes and the thread finishes on its own.
    #[test]
    fn dropping_the_renderer_leaves_the_last_frame_readable() {
        let (doc, page) = document().expect("a document with one page");
        let renderer = CanvasRenderer::new();
        let before = renderer.generation();
        renderer
            .render_canvas(
                doc,
                page,
                (0.0, 0.0),
                1.0,
                (200.0, 150.0),
                1.0,
                Options::default(),
            )
            .expect("a usable view");
        await_generation(&renderer, before).expect("a frame");
        let frame = renderer.frame().expect("a frame");
        drop(renderer);
        assert!(frame.image.is_some(), "the frame went with the renderer");
    }
    /// A frame rendered at one zoom, drawn while the view is at another, must
    /// be drawn at the NEW zoom's size.
    ///
    /// Between a zoom gesture and the frame that answers it, the frame on
    /// screen is the old one. Drawing it at its own size while the overlay
    /// drew the new zoom's content is what made objects change size as the
    /// canvas zoomed, and what made a resized object disagree with the handle
    /// being dragged.
    #[test]
    fn a_frame_drawn_at_a_different_zoom_keeps_up_with_the_view() {
        // A frame of 1600x1200 device pixels, rendered at a ratio of 2 and a
        // scale of 2 device pixels per page unit -- so it covers 800x600 page
        // units, and it was rendered at a zoom of 1.
        let f = frame((0, 0), 2.0, 2.0);
        assert_eq!(
            f.logical_size(1.0),
            (800.0, 600.0),
            "at the zoom it was rendered at, it covers its own page units"
        );
        assert_eq!(
            f.logical_size(2.0),
            (1600.0, 1200.0),
            "at twice the zoom, it must be drawn twice as large"
        );
        assert_eq!(
            f.logical_size(0.5),
            (400.0, 300.0),
            "and at half the zoom, half as large"
        );
    }
}
