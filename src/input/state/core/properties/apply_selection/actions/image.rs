use super::super::constants::{MAX_CORNER_RADIUS, SELECTION_CORNER_RADIUS_STEP};
use crate::draw::Shape;
use crate::input::state::core::base::InputState;

impl InputState {
    pub(in crate::input::state::core::properties) fn apply_selection_corner_radius(
        &mut self,
        direction: i32,
    ) -> bool {
        let delta = SELECTION_CORNER_RADIUS_STEP * direction as f64;
        let result = self.apply_selection_change(
            |shape| matches!(shape, Shape::Image { .. }),
            |shape| match shape {
                Shape::Image {
                    w,
                    h,
                    corner_radius,
                    ..
                } => {
                    // Cap at half the shorter side, which is where the corner
                    // arcs meet and the image reads as a pill. Stepping past it
                    // would keep incrementing a value with no visible effect.
                    let ceiling = (w.unsigned_abs().min(h.unsigned_abs()) as f64 / 2.0)
                        .min(MAX_CORNER_RADIUS);
                    let next = (*corner_radius + delta).clamp(0.0, ceiling);
                    if (next - *corner_radius).abs() > f64::EPSILON {
                        *corner_radius = next;
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            },
        );

        self.report_selection_apply_result(result, "corner radius")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BoardsConfig, KeybindingsConfig, PresenterModeConfig};
    use crate::draw::{Color, EmbeddedImage, FontDescriptor};
    use crate::input::state::core::properties::SelectionPropertyKind;
    use crate::input::{ClickHighlightSettings, EraserMode};

    fn make_state() -> InputState {
        let keybindings = KeybindingsConfig::default();
        let action_map = keybindings
            .build_action_map()
            .expect("default keybindings map");

        InputState::with_defaults(
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            4.0,
            4.0,
            EraserMode::Brush,
            0.32,
            false,
            32.0,
            FontDescriptor::default(),
            false,
            20.0,
            30.0,
            false,
            true,
            BoardsConfig::default(),
            action_map,
            usize::MAX,
            ClickHighlightSettings::disabled(),
            0,
            0,
            true,
            0,
            0,
            5,
            5,
            PresenterModeConfig::default(),
        )
    }

    fn image_shape(w: i32, h: i32, corner_radius: f64) -> Shape {
        Shape::Image {
            x: 0,
            y: 0,
            w,
            h,
            data: EmbeddedImage {
                mime_type: "image/png".to_string(),
                width: 10,
                height: 10,
                bytes: vec![1, 2, 3],
            },
            corner_radius,
        }
    }

    fn state_with_image(shape: Shape) -> (InputState, crate::draw::ShapeId) {
        let mut state = make_state();
        let id = state.boards.active_frame_mut().add_shape(shape);
        state.set_selection(vec![id]);
        (state, id)
    }

    fn radius(state: &InputState, id: crate::draw::ShapeId) -> f64 {
        match &state.boards.active_frame().shape(id).unwrap().shape {
            Shape::Image { corner_radius, .. } => *corner_radius,
            other => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn stepping_forward_rounds_the_corners() {
        let (mut state, id) = state_with_image(image_shape(200, 120, 0.0));

        assert!(state.apply_selection_corner_radius(1));

        assert_eq!(radius(&state, id), SELECTION_CORNER_RADIUS_STEP);
    }

    #[test]
    fn the_radius_never_goes_below_square() {
        let (mut state, id) = state_with_image(image_shape(200, 120, 0.0));

        assert!(!state.apply_selection_corner_radius(-1));

        assert_eq!(radius(&state, id), 0.0);
    }

    #[test]
    fn the_radius_stops_at_half_the_shorter_side() {
        // A 40px-tall image cannot show more than a 20px corner.
        let (mut state, id) = state_with_image(image_shape(200, 40, 18.0));

        assert!(state.apply_selection_corner_radius(1));

        assert_eq!(radius(&state, id), 20.0);
        assert!(!state.apply_selection_corner_radius(1));
    }

    #[test]
    fn non_image_shapes_do_not_expose_the_property() {
        let mut state = make_state();
        let id = state.boards.active_frame_mut().add_shape(Shape::Rect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
            fill: false,
            color: state.current_color,
            thick: 2.0,
        });

        let entries = state.build_selection_property_entries(&[id]);

        assert!(
            !entries
                .iter()
                .any(|entry| entry.kind == SelectionPropertyKind::CornerRadius)
        );
    }

    #[test]
    fn images_expose_the_property_with_a_readable_value() {
        let (state, id) = state_with_image(image_shape(200, 120, 0.0));

        let entries = state.build_selection_property_entries(&[id]);
        let entry = entries
            .iter()
            .find(|entry| entry.kind == SelectionPropertyKind::CornerRadius)
            .expect("corner radius entry");

        assert_eq!(entry.label, "Corner radius");
        assert_eq!(entry.value, "Square");
    }
}
