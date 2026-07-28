use crate::draw::shape::EmbeddedImage;
use crate::image_decode::{
    DecodedImage, EncodedImageFormat, decode_rgba, format_from_mime_or_bytes, gif,
};
use cairo::{Format, ImageSurface};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::Duration;

pub mod animation;

use animation::AnimatedImage;
pub use animation::{AnimationStatus, animation_playhead, set_animation_playhead};

const IMAGE_CACHE_ENTRIES: usize = 32;

/// Ceiling on decoded pixel data held by the cache. Stills are small enough that
/// the entry count governs them, but one animation can outweigh dozens of them,
/// so bytes are tracked as well.
const IMAGE_CACHE_BYTES: usize = 192 * 1024 * 1024;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ImageCacheKey {
    mime_type: String,
    len: usize,
    hash: u64,
    width: u32,
    height: u32,
}

/// A decoded image, or the memory of having failed to decode one.
#[derive(Debug)]
enum CachedImage {
    Still(Rc<ImageSurface>),
    Animated(Rc<AnimatedImage>),
    /// Decoding failed. Cached so a broken payload is not re-decoded on every
    /// frame for as long as its shape stays on the canvas.
    Failed,
}

impl CachedImage {
    fn decoded_bytes(&self) -> usize {
        match self {
            Self::Still(surface) => surface_bytes(surface),
            Self::Animated(animation) => animation.surface(0).map_or(0, |surface| {
                surface_bytes(surface) * animation.frame_count()
            }),
            Self::Failed => 0,
        }
    }
}

fn surface_bytes(surface: &ImageSurface) -> usize {
    (surface.stride().max(0) as usize).saturating_mul(surface.height().max(0) as usize)
}

thread_local! {
    static IMAGE_CACHE: RefCell<ImageSurfaceCache> = RefCell::new(ImageSurfaceCache::new());
}

struct ImageSurfaceCache {
    entries: HashMap<ImageCacheKey, Rc<CachedImage>>,
    order: VecDeque<ImageCacheKey>,
    bytes: usize,
}

impl ImageSurfaceCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }

    fn get(&mut self, key: &ImageCacheKey) -> Option<Rc<CachedImage>> {
        self.entries.get(key).cloned()
    }

    fn insert(&mut self, key: ImageCacheKey, image: Rc<CachedImage>) -> Rc<CachedImage> {
        if let Some(previous) = self.entries.insert(key.clone(), image.clone()) {
            self.bytes = self.bytes.saturating_sub(previous.decoded_bytes());
            self.bytes = self.bytes.saturating_add(image.decoded_bytes());
            return image;
        }

        self.order.push_back(key);
        self.bytes = self.bytes.saturating_add(image.decoded_bytes());
        // Keep at least one entry so the image being rendered right now survives
        // eviction even when it alone exceeds the byte budget.
        while self.order.len() > 1
            && (self.order.len() > IMAGE_CACHE_ENTRIES || self.bytes > IMAGE_CACHE_BYTES)
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(evicted.decoded_bytes());
            }
        }
        image
    }
}

pub fn render_image_shape(
    ctx: &cairo::Context,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    data: &EmbeddedImage,
) {
    if w == 0 || h == 0 {
        return;
    }
    let Some(surface) = current_surface(data) else {
        render_missing_image_placeholder(ctx, x, y, w, h);
        return;
    };

    let width = w.saturating_abs().max(1) as f64;
    let height = h.saturating_abs().max(1) as f64;
    let draw_x = if w < 0 { x + w } else { x };
    let draw_y = if h < 0 { y + h } else { y };

    let _ = ctx.save();
    ctx.rectangle(draw_x as f64, draw_y as f64, width, height);
    ctx.clip();
    ctx.translate(draw_x as f64, draw_y as f64);
    ctx.scale(
        width / surface.width().max(1) as f64,
        height / surface.height().max(1) as f64,
    );
    let _ = ctx.set_source_surface(surface.as_ref(), 0.0, 0.0);
    let _ = ctx.paint();
    let _ = ctx.restore();
}

/// Playback state for an image at the given position, or `None` when the image
/// is a still (or failed to decode) and therefore never needs a redraw.
///
/// Decodes on first use, so the caller drives when that cost is paid.
pub fn animation_status(data: &EmbeddedImage, playhead: Duration) -> Option<AnimationStatus> {
    match cached_image(data).as_ref() {
        CachedImage::Animated(animation) => Some(animation.status_at(playhead)),
        CachedImage::Still(_) | CachedImage::Failed => None,
    }
}

/// True when this payload could hold multiple frames, judged from the stored
/// MIME type and magic bytes alone. Cheap enough to call before decoding.
pub fn is_animatable(data: &EmbeddedImage) -> bool {
    format_from_mime_or_bytes(&data.mime_type, &data.bytes)
        .is_some_and(EncodedImageFormat::is_animatable)
}

/// The surface to paint for this image right now, honoring the shared playhead.
fn current_surface(data: &EmbeddedImage) -> Option<Rc<ImageSurface>> {
    match cached_image(data).as_ref() {
        CachedImage::Still(surface) => Some(surface.clone()),
        CachedImage::Animated(animation) => {
            let status = animation.status_at(animation_playhead());
            animation.surface(status.frame_index).cloned()
        }
        CachedImage::Failed => None,
    }
}

fn cached_image(data: &EmbeddedImage) -> Rc<CachedImage> {
    let key = ImageCacheKey {
        mime_type: data.mime_type.clone(),
        len: data.bytes.len(),
        hash: content_hash(&data.bytes),
        width: data.width,
        height: data.height,
    };

    IMAGE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(image) = cache.get(&key) {
            return image;
        }

        cache.insert(key, Rc::new(decode_image(data)))
    })
}

fn decode_image(data: &EmbeddedImage) -> CachedImage {
    let Some(format) = format_from_mime_or_bytes(&data.mime_type, &data.bytes) else {
        log::warn!(
            "Cannot render embedded image: unsupported MIME type {}",
            data.mime_type
        );
        return CachedImage::Failed;
    };

    if format.is_animatable()
        && let Some(animated) = decode_animated(data)
    {
        return CachedImage::Animated(Rc::new(animated));
    }

    match decode_rgba(format, &data.bytes).and_then(|image| {
        surface_from_rgba(&image).ok_or_else(|| "cairo rejected the decoded image".to_string())
    }) {
        Ok(surface) => CachedImage::Still(Rc::new(surface)),
        Err(err) => {
            log::warn!("Cannot render embedded {} image: {}", data.mime_type, err);
            CachedImage::Failed
        }
    }
}

/// Decodes every frame. Returns `None` for single-frame GIFs and for payloads
/// that blow the decode budget, both of which render fine as stills.
fn decode_animated(data: &EmbeddedImage) -> Option<AnimatedImage> {
    let animation = match gif::decode_animation(&data.bytes) {
        Ok(animation) => animation,
        Err(err) => {
            log::debug!(
                "Falling back to a still frame for {} image: {}",
                data.mime_type,
                err
            );
            return None;
        }
    };
    if animation.truncated {
        // Looping only the opening frames would misrepresent the image.
        log::warn!(
            "Animated {} image is too large to play ({}x{}, over the decode budget); showing its first frame",
            data.mime_type,
            animation.width,
            animation.height
        );
        return None;
    }

    let mut delays = Vec::with_capacity(animation.frames.len());
    let mut surfaces = Vec::with_capacity(animation.frames.len());
    for frame in &animation.frames {
        let decoded = DecodedImage {
            width: animation.width,
            height: animation.height,
            rgba: frame.rgba.clone(),
        };
        surfaces.push(Rc::new(surface_from_rgba(&decoded)?));
        delays.push(frame.delay);
    }

    let frame_count = surfaces.len();
    let animated = AnimatedImage::new(surfaces, &delays, animation.passes)?;
    log::info!(
        "Decoded animated {} image: {}x{}, frames={}, duration={}ms, loops={}",
        data.mime_type,
        animation.width,
        animation.height,
        frame_count,
        animated.total_duration().as_millis(),
        animation
            .passes
            .map_or_else(|| "forever".to_string(), |passes| passes.to_string())
    );
    Some(animated)
}

/// Converts non-premultiplied RGBA into a premultiplied ARGB32 cairo surface.
fn surface_from_rgba(image: &DecodedImage) -> Option<ImageSurface> {
    let width = image.width;
    let height = image.height;
    if width == 0 || height == 0 {
        return None;
    }

    let stride = Format::ARgb32.stride_for_width(width).ok()? as usize;
    let mut pixels = vec![0u8; stride * height as usize];
    for (row, source) in image.rgba.chunks_exact(width as usize * 4).enumerate() {
        let offset = row * stride;
        let row_bytes = &mut pixels[offset..offset + width as usize * 4];
        for (pixel, out) in source.chunks_exact(4).zip(row_bytes.chunks_exact_mut(4)) {
            let [r, g, b, a] = [pixel[0], pixel[1], pixel[2], pixel[3]];
            let premul =
                |channel: u8| -> u8 { ((channel as u16 * a as u16 + 127) / 255).min(255) as u8 };
            let r = premul(r);
            let g = premul(g);
            let b = premul(b);
            if cfg!(target_endian = "little") {
                out.copy_from_slice(&[b, g, r, a]);
            } else {
                out.copy_from_slice(&[a, r, g, b]);
            }
        }
    }

    ImageSurface::create_for_data(
        pixels,
        Format::ARgb32,
        width as i32,
        height as i32,
        stride as i32,
    )
    .ok()
}

/// Identifies a payload from a bounded sample rather than the whole buffer.
///
/// This runs on every cache lookup, which for an animation means once per shape
/// per rendered frame; hashing megabytes at that rate was measurable on its own.
/// The sample is combined with the exact length, dimensions, and MIME type in
/// `ImageCacheKey`, so two distinct images would have to agree on all of those
/// plus both ends of their encoded bytes to collide.
fn content_hash(bytes: &[u8]) -> u64 {
    const SAMPLE: usize = 4096;

    let mut hasher = DefaultHasher::new();
    bytes.len().hash(&mut hasher);
    let head = bytes.len().min(SAMPLE);
    bytes[..head].hash(&mut hasher);
    // Never re-hash bytes already covered by the head sample.
    let tail_start = bytes.len().saturating_sub(SAMPLE).max(head);
    bytes[tail_start..].hash(&mut hasher);
    hasher.finish()
}

fn render_missing_image_placeholder(ctx: &cairo::Context, x: i32, y: i32, w: i32, h: i32) {
    let width = w.saturating_abs().max(1) as f64;
    let height = h.saturating_abs().max(1) as f64;
    let draw_x = if w < 0 { x + w } else { x } as f64;
    let draw_y = if h < 0 { y + h } else { y } as f64;

    let _ = ctx.save();
    ctx.rectangle(draw_x, draw_y, width, height);
    ctx.set_source_rgba(0.12, 0.12, 0.12, 0.24);
    let _ = ctx.fill_preserve();
    ctx.set_source_rgba(0.9, 0.9, 0.9, 0.8);
    ctx.set_line_width(2.0);
    let _ = ctx.stroke();
    ctx.move_to(draw_x, draw_y);
    ctx.line_to(draw_x + width, draw_y + height);
    ctx.move_to(draw_x + width, draw_y);
    ctx.line_to(draw_x, draw_y + height);
    let _ = ctx.stroke();
    let _ = ctx.restore();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animated_gif() -> EmbeddedImage {
        let mut bytes = Vec::new();
        {
            let palette = [0xff, 0x00, 0x00, 0x00, 0x00, 0xff];
            let mut encoder = ::gif::Encoder::new(&mut bytes, 1, 1, &palette).unwrap();
            encoder.set_repeat(::gif::Repeat::Infinite).unwrap();
            for index in [0u8, 1] {
                let frame = ::gif::Frame {
                    width: 1,
                    height: 1,
                    buffer: vec![index].into(),
                    delay: 10,
                    ..Default::default()
                };
                encoder.write_frame(&frame).unwrap();
            }
        }
        EmbeddedImage {
            mime_type: "image/gif".to_string(),
            width: 1,
            height: 1,
            bytes,
        }
    }

    fn still_png() -> EmbeddedImage {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 0, 0xff, 0xff]).unwrap();
        }
        EmbeddedImage {
            mime_type: "image/png".to_string(),
            width: 1,
            height: 1,
            bytes,
        }
    }

    #[test]
    fn animated_gifs_report_playback_status() {
        let image = animated_gif();

        let start = animation_status(&image, Duration::ZERO).unwrap();
        assert_eq!(start.frame_index, 0);
        assert_eq!(start.time_to_next, Some(Duration::from_millis(100)));

        let later = animation_status(&image, Duration::from_millis(120)).unwrap();
        assert_eq!(later.frame_index, 1);
    }

    #[test]
    fn stills_never_report_playback_status() {
        assert!(animation_status(&still_png(), Duration::ZERO).is_none());
    }

    #[test]
    fn animatable_is_decided_by_format_not_decoding() {
        assert!(is_animatable(&animated_gif()));
        assert!(!is_animatable(&still_png()));
    }

    #[test]
    fn the_playhead_selects_which_frame_renders() {
        let image = animated_gif();

        set_animation_playhead(Duration::ZERO);
        let first = current_surface(&image).unwrap();
        set_animation_playhead(Duration::from_millis(120));
        let second = current_surface(&image).unwrap();
        set_animation_playhead(Duration::ZERO);

        assert!(!Rc::ptr_eq(&first, &second));
    }

    #[test]
    fn content_hash_separates_payloads_that_differ_at_either_end() {
        let mut head_differs = vec![0u8; 32 * 1024];
        head_differs[0] = 1;
        let mut tail_differs = vec![0u8; 32 * 1024];
        *tail_differs.last_mut().unwrap() = 1;
        let baseline = vec![0u8; 32 * 1024];

        assert_ne!(content_hash(&baseline), content_hash(&head_differs));
        assert_ne!(content_hash(&baseline), content_hash(&tail_differs));
        assert_ne!(content_hash(&baseline), content_hash(&vec![0u8; 31 * 1024]));
        assert_eq!(content_hash(&baseline), content_hash(&vec![0u8; 32 * 1024]));
    }

    #[test]
    fn undecodable_payloads_are_remembered_as_failed() {
        let broken = EmbeddedImage {
            mime_type: "image/png".to_string(),
            width: 1,
            height: 1,
            bytes: b"\x89PNG\r\n\x1a\nnot actually a png".to_vec(),
        };

        assert!(current_surface(&broken).is_none());
        assert!(matches!(
            cached_image(&broken).as_ref(),
            CachedImage::Failed
        ));
    }
}
