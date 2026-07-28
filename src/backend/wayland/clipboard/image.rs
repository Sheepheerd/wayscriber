use super::{
    ClipboardPasteResult, MAX_CLIPBOARD_IMAGE_PIXELS, WAYSCRIBER_SELECTION_MIME, file_list,
};
use crate::draw::EmbeddedImage;
use crate::image_decode::{decode_rgba, format_from_mime_or_bytes, image_dimensions};

pub(super) fn choose_supported_mime(offered: &[String]) -> Option<String> {
    // `image/gif` outranks `image/png`: browsers and chat clients offer both for
    // an animated GIF, and taking the PNG silently flattens it to one frame.
    if let Some(mime) = [
        WAYSCRIBER_SELECTION_MIME,
        "image/gif",
        "image/png",
        "image/jpeg",
        "image/jpg",
    ]
    .into_iter()
    .find(|candidate| offered.iter().any(|mime| mime == candidate))
    .map(ToString::to_string)
    {
        return Some(mime);
    }

    offered
        .iter()
        .find(|mime| file_list::is_uri_list_mime(mime))
        .cloned()
}

pub(super) fn decode_clipboard_image(mime_type: &str, bytes: Vec<u8>) -> ClipboardPasteResult {
    let encoded_bytes = bytes.len();
    let Some(format) = format_from_mime_or_bytes(mime_type, &bytes) else {
        return ClipboardPasteResult::DecodeFailed(format!("unsupported MIME type {}", mime_type));
    };
    let dimensions = match image_dimensions(format, &bytes) {
        Ok(dimensions) => dimensions,
        Err(err) => return ClipboardPasteResult::DecodeFailed(err),
    };
    let pixels = dimensions.0 as u64 * dimensions.1 as u64;
    if pixels > MAX_CLIPBOARD_IMAGE_PIXELS {
        return ClipboardPasteResult::TooManyPixels {
            width: dimensions.0,
            height: dimensions.1,
            limit: MAX_CLIPBOARD_IMAGE_PIXELS,
        };
    }
    if let Err(err) = decode_rgba(format, &bytes) {
        return ClipboardPasteResult::DecodeFailed(err);
    }
    log::info!(
        "Decoded clipboard image: offered_mime={}, stored_mime={}, dimensions={}x{}, encoded_bytes={}, animatable={}",
        mime_type,
        format.canonical_mime_type(),
        dimensions.0,
        dimensions.1,
        encoded_bytes,
        format.is_animatable()
    );
    ClipboardPasteResult::Image(EmbeddedImage {
        mime_type: format.canonical_mime_type().to_string(),
        width: dimensions.0,
        height: dimensions.1,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::choose_supported_mime;
    use crate::backend::wayland::clipboard::MAX_CLIPBOARD_IMAGE_BYTES;

    #[test]
    fn an_animated_gif_offer_wins_over_the_flattened_png_alongside_it() {
        let offered = vec![
            "text/html".to_string(),
            "image/png".to_string(),
            "image/gif".to_string(),
        ];

        assert_eq!(
            choose_supported_mime(&offered).as_deref(),
            Some("image/gif")
        );
    }

    #[test]
    fn png_is_still_chosen_when_no_gif_is_offered() {
        let offered = vec!["image/jpeg".to_string(), "image/png".to_string()];

        assert_eq!(
            choose_supported_mime(&offered).as_deref(),
            Some("image/png")
        );
    }

    #[test]
    fn image_byte_cap_leaves_room_for_default_persisted_create_history() {
        let encoded_len = MAX_CLIPBOARD_IMAGE_BYTES.div_ceil(3) * 4;
        let duplicated_history_len = encoded_len * 2;
        let default_session_budget = 50 * 1024 * 1024;
        let json_margin = 512 * 1024;

        assert!(duplicated_history_len + json_margin < default_session_budget);
    }
}
