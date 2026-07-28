pub(super) const SELECTION_THICKNESS_STEP: f64 = 1.0;
pub(super) const SELECTION_FONT_SIZE_STEP: f64 = 2.0;
pub(super) const SELECTION_ARROW_LENGTH_STEP: f64 = 2.0;
pub(super) const SELECTION_ARROW_ANGLE_STEP: f64 = 2.0;
pub(super) const MIN_FONT_SIZE: f64 = 8.0;
pub(super) const MAX_FONT_SIZE: f64 = 72.0;
pub(super) const MIN_ARROW_LENGTH: f64 = 5.0;
pub(super) const MAX_ARROW_LENGTH: f64 = 50.0;
pub(super) const MIN_ARROW_ANGLE: f64 = 15.0;
pub(super) const MAX_ARROW_ANGLE: f64 = 60.0;
pub(super) const SELECTION_CORNER_RADIUS_STEP: f64 = 4.0;
/// Upper bound independent of image size, so stepping a very large image does
/// not require hundreds of presses to reach a visible change.
pub(super) const MAX_CORNER_RADIUS: f64 = 256.0;
