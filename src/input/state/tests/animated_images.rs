use super::*;
use crate::draw::EmbeddedImage;
use std::time::{Duration, Instant};

/// Two-frame GIF, 100ms per frame, looping forever.
fn animated_gif_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let palette = [0xff, 0x00, 0x00, 0x00, 0x00, 0xff];
        let mut encoder = gif::Encoder::new(&mut bytes, 1, 1, &palette).unwrap();
        encoder.set_repeat(gif::Repeat::Infinite).unwrap();
        for index in [0u8, 1] {
            let frame = gif::Frame {
                width: 1,
                height: 1,
                buffer: vec![index].into(),
                delay: 10,
                ..Default::default()
            };
            encoder.write_frame(&frame).unwrap();
        }
    }
    bytes
}

fn add_image(state: &mut InputState, mime_type: &str, bytes: Vec<u8>) -> crate::draw::ShapeId {
    state.boards.active_frame_mut().add_shape(Shape::Image {
        x: 10,
        y: 20,
        w: 64,
        h: 64,
        data: EmbeddedImage {
            mime_type: mime_type.to_string(),
            width: 1,
            height: 1,
            bytes,
        },
        corner_radius: 0.0,
    })
}

fn still_png_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0, 0, 0xff, 0xff]).unwrap();
    }
    bytes
}

#[test]
fn a_canvas_without_animated_images_schedules_nothing() {
    let mut state = create_test_input_state();
    add_image(&mut state, "image/png", still_png_bytes());
    let now = Instant::now();

    assert!(!state.advance_animated_images(now));
    assert_eq!(state.animated_image_timeout(now), None);
}

#[test]
fn an_animated_gif_schedules_its_next_frame() {
    let mut state = create_test_input_state();
    let id = add_image(&mut state, "image/gif", animated_gif_bytes());
    let now = Instant::now();

    assert!(state.advance_animated_images(now));
    assert_eq!(state.animated_image_frame(id), Some(0));
    // The first frame lasts 100ms, so that is when the loop must wake.
    assert_eq!(
        state.animated_image_timeout(now),
        Some(Duration::from_millis(100))
    );
}

#[test]
fn advancing_past_a_frame_boundary_flips_the_frame_and_requests_a_redraw() {
    let mut state = create_test_input_state();
    let id = add_image(&mut state, "image/gif", animated_gif_bytes());
    let start = Instant::now();
    state.advance_animated_images(start);
    state.needs_redraw = false;
    state.take_dirty_region_report();

    // Still inside frame zero: nothing to repaint.
    state.advance_animated_images(start + Duration::from_millis(50));
    assert_eq!(state.animated_image_frame(id), Some(0));
    assert!(!state.needs_redraw);

    // Past the boundary: the frame changes and its bounds are damaged.
    state.advance_animated_images(start + Duration::from_millis(150));
    assert_eq!(state.animated_image_frame(id), Some(1));
    assert!(state.needs_redraw);
    assert!(!state.take_dirty_region_report().regions.is_empty());
}

#[test]
fn playback_loops_back_to_the_first_frame() {
    let mut state = create_test_input_state();
    let id = add_image(&mut state, "image/gif", animated_gif_bytes());
    let start = Instant::now();
    state.advance_animated_images(start);

    state.advance_animated_images(start + Duration::from_millis(150));
    assert_eq!(state.animated_image_frame(id), Some(1));
    // 200ms is one full loop.
    state.advance_animated_images(start + Duration::from_millis(200));
    assert_eq!(state.animated_image_frame(id), Some(0));
}

#[test]
fn disabling_playback_holds_the_first_frame() {
    let mut state = create_test_input_state();
    state.animated_images_enabled = false;
    add_image(&mut state, "image/gif", animated_gif_bytes());
    let now = Instant::now();

    assert!(!state.advance_animated_images(now));
    assert_eq!(state.animated_image_timeout(now), None);
}

#[test]
fn removing_the_last_animated_image_stops_the_schedule() {
    let mut state = create_test_input_state();
    let id = add_image(&mut state, "image/gif", animated_gif_bytes());
    let now = Instant::now();
    state.advance_animated_images(now);
    assert!(state.animated_image_timeout(now).is_some());

    state.boards.active_frame_mut().remove_shape_by_id(id);

    assert!(!state.advance_animated_images(now));
    assert_eq!(state.animated_image_timeout(now), None);
    assert_eq!(state.animated_image_frame(id), None);
}

#[test]
fn an_expired_deadline_is_rearmed_so_a_suppressed_redraw_cannot_spin_the_loop() {
    let mut state = create_test_input_state();
    add_image(&mut state, "image/gif", animated_gif_bytes());
    let start = Instant::now();
    state.advance_animated_images(start);

    let due = start + Duration::from_millis(500);
    assert!(state.animated_images_due(due));
    // Without a render to recompute it, the deadline still moves forward.
    assert!(!state.animated_images_due(due));
    assert!(state.animated_image_timeout(due) > Some(Duration::ZERO));
}

#[test]
fn playback_starts_at_frame_zero_for_the_first_animation_on_a_canvas() {
    let mut state = create_test_input_state();
    // Let the shared clock run well past a loop before anything is animated.
    let start = Instant::now();
    state.advance_animated_images(start);
    let id = add_image(&mut state, "image/gif", animated_gif_bytes());

    state.advance_animated_images(start + Duration::from_secs(37));

    assert_eq!(state.animated_image_frame(id), Some(0));
}
