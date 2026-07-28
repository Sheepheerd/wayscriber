//! Playback timing for animated image shapes.
//!
//! Frames live in the render-side surface cache; this module owns the clock and
//! the elapsed-time-to-frame mapping so both the renderer and the redraw
//! scheduler agree on which frame is current.

use cairo::ImageSurface;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    /// Playback position shared by every animated image in the current pass.
    ///
    /// Set once per rendered frame so that all animations advance together and
    /// so exports capture exactly the frame that was last on screen, rather than
    /// re-deriving a time of their own.
    static PLAYHEAD: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

/// Positions playback for all subsequent renders on this thread.
pub fn set_animation_playhead(playhead: Duration) {
    PLAYHEAD.with(|cell| cell.set(playhead));
}

/// Current playback position.
pub fn animation_playhead() -> Duration {
    PLAYHEAD.with(Cell::get)
}

/// Which frame an animation shows, and how long until it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationStatus {
    pub frame_index: usize,
    /// Time until the next frame is due; `None` once playback has finished and
    /// the animation is holding its final frame forever.
    pub time_to_next: Option<Duration>,
}

/// Decoded frames plus the timeline needed to index them.
#[derive(Debug)]
pub struct AnimatedImage {
    frames: Vec<Rc<ImageSurface>>,
    /// Cumulative end time of each frame; the last entry equals `total`.
    ends: Vec<Duration>,
    total: Duration,
    /// How many times the animation plays; `None` loops forever.
    passes: Option<u32>,
}

impl AnimatedImage {
    /// Returns `None` unless there are at least two frames with real duration —
    /// anything else is a still image and should take the cheaper path.
    pub fn new(
        frames: Vec<Rc<ImageSurface>>,
        delays: &[Duration],
        passes: Option<u32>,
    ) -> Option<Self> {
        if frames.len() < 2 || frames.len() != delays.len() {
            return None;
        }
        let mut ends = Vec::with_capacity(delays.len());
        let mut total = Duration::ZERO;
        for delay in delays {
            total = total.saturating_add(*delay);
            ends.push(total);
        }
        if total.is_zero() {
            return None;
        }
        Some(Self {
            frames,
            ends,
            total,
            passes,
        })
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn total_duration(&self) -> Duration {
        self.total
    }

    pub fn surface(&self, frame_index: usize) -> Option<&Rc<ImageSurface>> {
        self.frames.get(frame_index)
    }

    /// Maps a playback position onto a frame.
    pub fn status_at(&self, playhead: Duration) -> AnimationStatus {
        if let Some(passes) = self.passes {
            let playable = self.total.saturating_mul(passes);
            if playhead >= playable {
                // Finished: hold the last frame and stop asking for redraws.
                return AnimationStatus {
                    frame_index: self.frames.len() - 1,
                    time_to_next: None,
                };
            }
        }

        let position = self.position_in_loop(playhead);
        let frame_index = self
            .ends
            .partition_point(|end| *end <= position)
            .min(self.frames.len() - 1);
        let time_to_next = self.ends[frame_index].saturating_sub(position);
        AnimationStatus {
            frame_index,
            // A zero-length wait would spin the event loop; the timeline
            // guarantees non-zero frame delays, so clamp defensively only.
            time_to_next: Some(time_to_next.max(Duration::from_millis(1))),
        }
    }

    fn position_in_loop(&self, playhead: Duration) -> Duration {
        let total = self.total.as_nanos();
        if total == 0 {
            return Duration::ZERO;
        }
        let remainder = playhead.as_nanos() % total;
        Duration::from_nanos(remainder.min(u128::from(u64::MAX)) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairo::Format;

    fn surface() -> Rc<ImageSurface> {
        Rc::new(ImageSurface::create(Format::ARgb32, 1, 1).unwrap())
    }

    fn animation(passes: Option<u32>) -> AnimatedImage {
        let delays = [
            Duration::from_millis(100),
            Duration::from_millis(50),
            Duration::from_millis(250),
        ];
        AnimatedImage::new(vec![surface(), surface(), surface()], &delays, passes).unwrap()
    }

    #[test]
    fn a_single_frame_is_not_an_animation() {
        assert!(AnimatedImage::new(vec![surface()], &[Duration::from_millis(100)], None).is_none());
    }

    #[test]
    fn frames_without_duration_are_not_an_animation() {
        assert!(
            AnimatedImage::new(
                vec![surface(), surface()],
                &[Duration::ZERO, Duration::ZERO],
                None
            )
            .is_none()
        );
    }

    #[test]
    fn playhead_maps_onto_the_frame_that_covers_it() {
        let animation = animation(None);

        assert_eq!(animation.status_at(Duration::ZERO).frame_index, 0);
        assert_eq!(
            animation.status_at(Duration::from_millis(99)).frame_index,
            0
        );
        // Exactly on a boundary belongs to the next frame.
        assert_eq!(
            animation.status_at(Duration::from_millis(100)).frame_index,
            1
        );
        assert_eq!(
            animation.status_at(Duration::from_millis(149)).frame_index,
            1
        );
        assert_eq!(
            animation.status_at(Duration::from_millis(150)).frame_index,
            2
        );
        assert_eq!(
            animation.status_at(Duration::from_millis(399)).frame_index,
            2
        );
    }

    #[test]
    fn playback_wraps_after_the_final_frame() {
        let animation = animation(None);

        // 400ms is one full loop, so it lands back on frame zero.
        assert_eq!(
            animation.status_at(Duration::from_millis(400)).frame_index,
            0
        );
        assert_eq!(
            animation
                .status_at(Duration::from_millis(1_000_000_500))
                .frame_index,
            1
        );
    }

    #[test]
    fn time_to_next_counts_down_within_a_frame() {
        let animation = animation(None);

        assert_eq!(
            animation.status_at(Duration::from_millis(30)).time_to_next,
            Some(Duration::from_millis(70))
        );
        assert_eq!(
            animation.status_at(Duration::from_millis(399)).time_to_next,
            Some(Duration::from_millis(1))
        );
    }

    #[test]
    fn finite_playback_holds_the_last_frame_and_stops_scheduling() {
        let animation = animation(Some(2));

        // Still looping through the second pass.
        assert_eq!(
            animation.status_at(Duration::from_millis(500)).frame_index,
            1
        );
        // Both passes are done at 800ms.
        let finished = animation.status_at(Duration::from_millis(800));
        assert_eq!(finished.frame_index, 2);
        assert_eq!(finished.time_to_next, None);
    }

    #[test]
    fn playhead_is_shared_across_the_pass() {
        set_animation_playhead(Duration::from_millis(250));

        assert_eq!(animation_playhead(), Duration::from_millis(250));

        set_animation_playhead(Duration::ZERO);
    }
}
