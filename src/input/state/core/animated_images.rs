//! Playback scheduling for animated image shapes.
//!
//! Rendering an animation frame is cheap; deciding *when* to render it is the
//! work. Each pass positions the shared playhead, damages only the images whose
//! frame actually changed, and reports when the next frame is due so the event
//! loop can sleep until then instead of polling.

use super::base::InputState;
use crate::draw::ShapeId;
use crate::draw::render::{animation_status, is_animatable, set_animation_playhead};
use crate::draw::shape::Shape;
use crate::util::Rect;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Floor on how often the event loop re-arms for animation. Frame delays are
/// already clamped well above this; it exists so a state where redraws are
/// suppressed (an overlay that is hidden mid-animation) cannot spin the loop on
/// a deadline that never advances.
const MIN_REARM: Duration = Duration::from_millis(16);

/// Slack around a repainted image, covering edge pixels that scaling can bleed
/// slightly outside the shape's exact bounds.
const FRAME_DAMAGE_PADDING: i32 = 2;

#[derive(Debug, Default)]
pub(crate) struct AnimatedImagePlayback {
    /// Origin of the shared playhead. Taken when the first animated image
    /// appears, so a GIF pasted onto a canvas without one starts at frame zero
    /// rather than partway through its loop.
    epoch: Option<Instant>,
    /// Frame currently on screen per shape, so a pass that changes nothing
    /// emits no damage.
    frames: HashMap<ShapeId, usize>,
    /// When the earliest tracked image next needs a different frame.
    next_flip: Option<Instant>,
}

impl AnimatedImagePlayback {
    fn clear(&mut self) {
        self.epoch = None;
        self.frames.clear();
        self.next_flip = None;
    }
}

/// One tracked image, resolved against the playhead in a borrow-free pass.
struct FrameUpdate {
    id: ShapeId,
    frame_index: usize,
    bounds: Option<Rect>,
    time_to_next: Option<Duration>,
}

impl InputState {
    /// Advances animated images and damages the ones whose frame changed.
    ///
    /// Returns whether any animation is still playing. Call once per render,
    /// before dirty regions are collected.
    pub fn advance_animated_images(&mut self, now: Instant) -> bool {
        if !self.animated_images_enabled {
            return false;
        }

        // The epoch is only meaningful once something is animating, so probe the
        // page against the existing (or provisional) origin first.
        let epoch = self.animated_images.epoch.unwrap_or(now);
        let playhead = now.saturating_duration_since(epoch);
        set_animation_playhead(playhead);

        let updates = self.collect_animation_updates(playhead);
        if updates.is_empty() {
            self.animated_images.clear();
            return false;
        }
        self.animated_images.epoch = Some(epoch);

        let mut next_flip: Option<Instant> = None;
        let mut damage: Vec<Option<Rect>> = Vec::new();
        let mut frames = HashMap::with_capacity(updates.len());
        for update in updates {
            if let Some(time_to_next) = update.time_to_next {
                let due = now + time_to_next;
                next_flip = Some(next_flip.map_or(due, |current: Instant| current.min(due)));
            }
            let changed = self.animated_images.frames.get(&update.id) != Some(&update.frame_index);
            if changed {
                damage.push(update.bounds);
            }
            frames.insert(update.id, update.frame_index);
        }

        self.animated_images.frames = frames;
        self.animated_images.next_flip = next_flip;

        if !damage.is_empty() {
            for bounds in damage.into_iter().flatten() {
                // Marked directly rather than through `mark_selection_dirty_region`:
                // that also flags the properties panel for a refresh, which must
                // not happen once per animation frame.
                let rect = bounds.inflated(FRAME_DAMAGE_PADDING).unwrap_or(bounds);
                self.dirty_tracker.mark_rect(rect);
            }
            self.needs_redraw = true;
        }

        next_flip.is_some()
    }

    /// Resolves every animated image on the active page against the playhead.
    ///
    /// Separated from the mutation above so the frame borrow ends before the
    /// dirty tracker is touched.
    fn collect_animation_updates(&self, playhead: Duration) -> Vec<FrameUpdate> {
        self.boards
            .active_frame()
            .shapes
            .iter()
            .filter_map(|drawn| {
                let Shape::Image { data, .. } = &drawn.shape else {
                    return None;
                };
                // Cheap format check first: this runs for every shape on every
                // rendered frame, while `animation_status` may decode.
                if !is_animatable(data) {
                    return None;
                }
                let status = animation_status(data, playhead)?;
                Some(FrameUpdate {
                    id: drawn.id,
                    frame_index: status.frame_index,
                    bounds: drawn.bounding_box(),
                    time_to_next: status.time_to_next,
                })
            })
            .collect()
    }

    /// Time until the next animation frame is due, for the event-loop timeout.
    pub fn animated_image_timeout(&self, now: Instant) -> Option<Duration> {
        self.animated_images
            .next_flip
            .map(|next| next.saturating_duration_since(now))
    }

    /// Whether a frame is due now. Re-arms the deadline so a suppressed redraw
    /// cannot leave the loop spinning on an expired one.
    pub fn animated_images_due(&mut self, now: Instant) -> bool {
        let Some(next_flip) = self.animated_images.next_flip else {
            return false;
        };
        if now < next_flip {
            return false;
        }
        self.animated_images.next_flip = Some(now + MIN_REARM);
        true
    }

    #[cfg(test)]
    pub(crate) fn animated_image_frame(&self, id: ShapeId) -> Option<usize> {
        self.animated_images.frames.get(&id).copied()
    }
}
