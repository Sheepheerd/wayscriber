//! GIF decoding, including multi-frame animation composition.
//!
//! The `gif` crate hands back raw sub-rectangles plus a disposal method; it does
//! not composite. This module owns that composition so callers receive whole
//! logical-screen RGBA frames that can be blitted directly.

use super::DecodedImage;
use gif::{ColorOutput, DecodeOptions, DisposalMethod, Repeat};
use std::io::Cursor;
use std::time::Duration;

/// Frames past this count are refused rather than decoded. Long-running screen
/// recordings turned into GIFs can otherwise consume gigabytes once expanded to
/// full-screen RGBA.
pub(crate) const MAX_ANIMATION_FRAMES: usize = 600;

/// Ceiling on the decoded (uncompressed) size of one animation. A GIF that
/// exceeds it still pastes; it just renders as its first frame.
pub(crate) const MAX_ANIMATION_DECODED_BYTES: usize = 96 * 1024 * 1024;

/// Delay applied when a frame requests 0 or 1 centiseconds. Encoders use those
/// values to mean "as fast as possible"; every browser substitutes 100ms, and
/// honoring them literally turns ordinary GIFs into strobe lights.
const DEFAULT_FRAME_DELAY: Duration = Duration::from_millis(100);
const MIN_FRAME_DELAY_CENTISECONDS: u16 = 2;

#[derive(Debug)]
pub(crate) struct GifFrame {
    /// Whole logical screen, RGBA8, non-premultiplied.
    pub(crate) rgba: Vec<u8>,
    /// How long this frame stays on screen.
    pub(crate) delay: Duration,
}

#[derive(Debug)]
pub(crate) struct GifAnimation {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) frames: Vec<GifFrame>,
    /// Number of times the animation plays; `None` loops forever.
    pub(crate) passes: Option<u32>,
    /// Set when decoding stopped at the frame budget rather than at the end of
    /// the animation. Playback must not loop a truncated animation — a long GIF
    /// replaying only its opening frames reads as a bug — so callers that want
    /// motion should fall back to a still frame.
    pub(crate) truncated: bool,
}

pub(crate) fn dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    let decoder = reader(bytes)?;
    let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
    if width == 0 || height == 0 {
        return Err("GIF logical screen has zero area".to_string());
    }
    Ok((width, height))
}

/// Decodes only the first frame, for callers that render a still image.
pub(crate) fn decode_first_frame(bytes: &[u8]) -> Result<DecodedImage, String> {
    let mut animation = decode_animation_limited(bytes, 1, MAX_ANIMATION_DECODED_BYTES)?;
    let frame = if animation.frames.is_empty() {
        return Err("GIF contains no frames".to_string());
    } else {
        animation.frames.swap_remove(0)
    };
    Ok(DecodedImage {
        width: animation.width,
        height: animation.height,
        rgba: frame.rgba,
    })
}

/// Decodes every frame, composited onto the logical screen.
pub(crate) fn decode_animation(bytes: &[u8]) -> Result<GifAnimation, String> {
    decode_animation_limited(bytes, MAX_ANIMATION_FRAMES, MAX_ANIMATION_DECODED_BYTES)
}

pub(crate) fn decode_animation_limited(
    bytes: &[u8],
    max_frames: usize,
    max_decoded_bytes: usize,
) -> Result<GifAnimation, String> {
    let mut decoder = reader(bytes)?;
    let width = u32::from(decoder.width());
    let height = u32::from(decoder.height());
    if width == 0 || height == 0 {
        return Err("GIF logical screen has zero area".to_string());
    }
    let frame_bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "GIF dimensions are too large".to_string())?;
    if frame_bytes > max_decoded_bytes {
        return Err(format!(
            "GIF frame needs {frame_bytes} bytes, over the {max_decoded_bytes} byte budget"
        ));
    }
    let frame_budget = (max_decoded_bytes / frame_bytes.max(1)).min(max_frames);

    let mut canvas = vec![0u8; frame_bytes];
    let mut frames: Vec<GifFrame> = Vec::new();
    let mut truncated = false;

    loop {
        let frame = match decoder.read_next_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(err) => {
                // Truncated animations are common in the wild. Keep whatever
                // decoded cleanly rather than dropping the paste entirely.
                if frames.is_empty() {
                    return Err(err.to_string());
                }
                log::debug!("Stopping GIF decode after {} frames: {}", frames.len(), err);
                break;
            }
        };

        let restore = (frame.dispose == DisposalMethod::Previous).then(|| canvas.clone());
        let region = FrameRegion::clamp(
            frame.left,
            frame.top,
            frame.width,
            frame.height,
            width,
            height,
        );
        blit_over(&mut canvas, width, &region, &frame.buffer);

        frames.push(GifFrame {
            rgba: canvas.clone(),
            delay: frame_delay(frame.delay),
        });

        match frame.dispose {
            DisposalMethod::Background => clear_region(&mut canvas, width, &region),
            DisposalMethod::Previous => {
                if let Some(restore) = restore {
                    canvas = restore;
                }
            }
            DisposalMethod::Keep | DisposalMethod::Any => {}
        }

        // Decode one frame past the budget so a complete animation that exactly
        // fills it is not misreported as truncated. The extra frame is dropped.
        if frames.len() > frame_budget {
            frames.pop();
            truncated = true;
            break;
        }
    }

    if frames.is_empty() {
        return Err("GIF contains no frames".to_string());
    }

    Ok(GifAnimation {
        width,
        height,
        frames,
        passes: passes_from_repeat(decoder.repeat()),
        truncated,
    })
}

fn reader(bytes: &[u8]) -> Result<gif::Decoder<Cursor<&[u8]>>, String> {
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    options
        .read_info(Cursor::new(bytes))
        .map_err(|err| err.to_string())
}

/// Sub-rectangle of the logical screen that a GIF frame writes into, already
/// clipped to the canvas. Frames may legally extend past the logical screen.
struct FrameRegion {
    left: usize,
    top: usize,
    /// Frame width as encoded, needed to stride through the source buffer.
    source_width: usize,
    /// Visible width and height after clipping.
    width: usize,
    height: usize,
}

impl FrameRegion {
    fn clamp(
        left: u16,
        top: u16,
        frame_width: u16,
        frame_height: u16,
        canvas_width: u32,
        canvas_height: u32,
    ) -> Self {
        let canvas_width = canvas_width as usize;
        let canvas_height = canvas_height as usize;
        let left = usize::from(left).min(canvas_width);
        let top = usize::from(top).min(canvas_height);
        Self {
            left,
            top,
            source_width: usize::from(frame_width),
            width: usize::from(frame_width).min(canvas_width.saturating_sub(left)),
            height: usize::from(frame_height).min(canvas_height.saturating_sub(top)),
        }
    }
}

/// Draws a frame onto the canvas. GIF transparency is a 1-bit mask, so a fully
/// transparent source pixel leaves the canvas pixel untouched.
fn blit_over(canvas: &mut [u8], canvas_width: u32, region: &FrameRegion, source: &[u8]) {
    let canvas_width = canvas_width as usize;
    for row in 0..region.height {
        let source_start = row * region.source_width * 4;
        let Some(source_row) = source.get(source_start..source_start + region.width * 4) else {
            break;
        };
        let target_start = ((region.top + row) * canvas_width + region.left) * 4;
        let Some(target_row) = canvas.get_mut(target_start..target_start + region.width * 4) else {
            break;
        };
        for (source_pixel, target_pixel) in source_row
            .chunks_exact(4)
            .zip(target_row.chunks_exact_mut(4))
        {
            if source_pixel[3] != 0 {
                target_pixel.copy_from_slice(source_pixel);
            }
        }
    }
}

fn clear_region(canvas: &mut [u8], canvas_width: u32, region: &FrameRegion) {
    let canvas_width = canvas_width as usize;
    for row in 0..region.height {
        let start = ((region.top + row) * canvas_width + region.left) * 4;
        if let Some(target_row) = canvas.get_mut(start..start + region.width * 4) {
            target_row.fill(0);
        }
    }
}

fn frame_delay(centiseconds: u16) -> Duration {
    if centiseconds < MIN_FRAME_DELAY_CENTISECONDS {
        return DEFAULT_FRAME_DELAY;
    }
    Duration::from_millis(u64::from(centiseconds) * 10)
}

/// Maps the NETSCAPE loop count to a playback pass count.
///
/// The decoder reports an explicit loop count of 0 as `Infinite`, so a
/// `Finite(0)` here means the GIF carried no loop extension at all, which every
/// browser plays exactly once.
fn passes_from_repeat(repeat: Repeat) -> Option<u32> {
    match repeat {
        Repeat::Infinite => None,
        Repeat::Finite(count) => Some(u32::from(count).max(1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a 2x1 GIF whose two frames each paint one pixel, so composition,
    /// offsets, and disposal are all observable in the output.
    fn two_frame_gif(dispose: DisposalMethod, delay: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let palette = [0xff, 0x00, 0x00, 0x00, 0x00, 0xff];
            let mut encoder = gif::Encoder::new(&mut bytes, 2, 1, &palette).unwrap();
            encoder.set_repeat(Repeat::Infinite).unwrap();

            let first = gif::Frame {
                width: 1,
                height: 1,
                buffer: vec![0].into(),
                delay,
                dispose,
                ..Default::default()
            };
            encoder.write_frame(&first).unwrap();

            let second = gif::Frame {
                left: 1,
                width: 1,
                height: 1,
                buffer: vec![1].into(),
                delay,
                ..Default::default()
            };
            encoder.write_frame(&second).unwrap();
        }
        bytes
    }

    #[test]
    fn frames_composite_onto_the_logical_screen() {
        let animation = decode_animation(&two_frame_gif(DisposalMethod::Keep, 5)).unwrap();

        assert_eq!((animation.width, animation.height), (2, 1));
        assert_eq!(animation.frames.len(), 2);
        // First frame paints only the left pixel; the right stays transparent.
        assert_eq!(animation.frames[0].rgba, [0xff, 0, 0, 0xff, 0, 0, 0, 0]);
        // `Keep` disposal leaves the left pixel in place under the second frame.
        assert_eq!(
            animation.frames[1].rgba,
            [0xff, 0, 0, 0xff, 0, 0, 0xff, 0xff]
        );
    }

    #[test]
    fn background_disposal_clears_the_previous_frame_region() {
        let animation = decode_animation(&two_frame_gif(DisposalMethod::Background, 5)).unwrap();

        // The left pixel is restored to transparent before frame two draws.
        assert_eq!(animation.frames[1].rgba, [0, 0, 0, 0, 0, 0, 0xff, 0xff]);
    }

    #[test]
    fn zero_and_one_centisecond_delays_become_100ms() {
        for delay in [0, 1] {
            let animation = decode_animation(&two_frame_gif(DisposalMethod::Keep, delay)).unwrap();
            assert_eq!(animation.frames[0].delay, DEFAULT_FRAME_DELAY);
        }
    }

    #[test]
    fn honest_delays_are_preserved() {
        let animation = decode_animation(&two_frame_gif(DisposalMethod::Keep, 5)).unwrap();

        assert_eq!(animation.frames[0].delay, Duration::from_millis(50));
    }

    #[test]
    fn infinite_repeat_maps_to_endless_playback() {
        let animation = decode_animation(&two_frame_gif(DisposalMethod::Keep, 5)).unwrap();

        assert_eq!(animation.passes, None);
    }

    #[test]
    fn decode_budget_limits_frame_count_and_reports_truncation() {
        let animation =
            decode_animation_limited(&two_frame_gif(DisposalMethod::Keep, 5), 1, usize::MAX)
                .unwrap();

        assert_eq!(animation.frames.len(), 1);
        assert!(animation.truncated);
    }

    #[test]
    fn an_animation_that_exactly_fills_the_budget_is_not_truncated() {
        let animation =
            decode_animation_limited(&two_frame_gif(DisposalMethod::Keep, 5), 2, usize::MAX)
                .unwrap();

        assert_eq!(animation.frames.len(), 2);
        assert!(!animation.truncated);
    }

    #[test]
    fn decode_first_frame_returns_the_logical_screen() {
        let image = decode_first_frame(&two_frame_gif(DisposalMethod::Keep, 5)).unwrap();

        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba, [0xff, 0, 0, 0xff, 0, 0, 0, 0]);
    }

    #[test]
    fn oversized_frames_are_rejected_before_allocation() {
        let err =
            decode_animation_limited(&two_frame_gif(DisposalMethod::Keep, 5), 8, 4).unwrap_err();

        assert!(err.contains("budget"), "unexpected error: {err}");
    }
}
