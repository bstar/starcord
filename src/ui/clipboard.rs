//! The system clipboard, in both directions.
//!
//! Text out is three lines and lives here so that the two places that copy
//! something spell it the same way. A picture in is the interesting half.
//!
//! ## Why a fresh connection every time
//!
//! Under Wayland the clipboard is owned by a live connection, and holding one
//! open for the life of the program means holding a socket for a feature used
//! a few times an hour. Under X11 `wayland-data-control` is not in play at all.
//! Failure is a note rather than an error either way: a terminal with no
//! clipboard at the other end — over ssh, in a bare tty — is a perfectly
//! ordinary place to be running a chat client.
//!
//! ## Why the picture becomes a PNG here
//!
//! `arboard` hands back raw RGBA and a size. Discord wants a file with a name
//! and a content type, so something has to encode it, and doing it at the
//! edge — before it becomes a chip — means the size shown on the chip is the
//! size that will actually be uploaded rather than a guess at it.

use std::borrow::Cow;

/// Put text on the clipboard.
pub fn copy(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text.to_string()))
        .map_err(|e| e.to_string())
}

/// The text on the clipboard, if that is what is there.
pub fn text() -> Result<String, String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map_err(|e| e.to_string())
}

/// The files on the clipboard: what a copy in a file manager, or "copy
/// image" in a desktop client that keeps its pictures as files, puts there.
pub fn files() -> Result<Vec<std::path::PathBuf>, String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.get().file_list())
        .map_err(|e| e.to_string())
}

/// A picture off the clipboard, as PNG bytes and its size in pixels.
pub fn image() -> Result<(Vec<u8>, (u32, u32)), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let image = clipboard.get_image().map_err(|e| e.to_string())?;
    let (w, h) = (image.width as u32, image.height as u32);
    let bytes = encode_png(w, h, &image.bytes)?;
    Ok((bytes, (w, h)))
}

/// RGBA, as a PNG.
///
/// Separated from the clipboard call so that it can be tested without one:
/// there is no clipboard in a test runner, and the half of this that can go
/// wrong is the encoding.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("the clipboard held a picture with no pixels in it".into());
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or("the clipboard held a picture too large to encode")?;
    if rgba.len() < expected {
        return Err("the clipboard held an incomplete picture".into());
    }
    let owned: Cow<'_, [u8]> = Cow::Borrowed(&rgba[..expected]);
    let buffer = image::RgbaImage::from_raw(width, height, owned.into_owned())
        .ok_or("the clipboard held an incomplete picture")?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two by two, out and back again. What this is really asserting is that
    /// what goes to Discord is a file a decoder recognises, with the pixels
    /// that were on the clipboard and not a transposed or shifted copy of
    /// them.
    #[test]
    fn a_picture_off_the_clipboard_round_trips_through_png() {
        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 128, // half-transparent white
        ];
        let png = encode_png(2, 2, &pixels).expect("two by two encodes");
        assert_eq!(&png[1..4], b"PNG", "that is not a PNG");

        let decoded = image::load_from_memory(&png).expect("and decodes again");
        let rgba = decoded.to_rgba8();
        assert_eq!((rgba.width(), rgba.height()), (2, 2));
        assert_eq!(rgba.as_raw().as_slice(), pixels.as_slice());
    }

    /// Everything that can be wrong with what a clipboard hands over is an
    /// error rather than a panic: the bytes come from another application.
    #[test]
    fn a_bad_picture_is_refused_rather_than_believed() {
        assert!(encode_png(0, 4, &[0; 16]).is_err());
        assert!(encode_png(4, 0, &[0; 16]).is_err());
        assert!(encode_png(4, 4, &[0; 8]).is_err(), "too few bytes");
        assert!(encode_png(u32::MAX, u32::MAX, &[0; 8]).is_err(), "overflow");
        // More bytes than the size calls for is a stride the caller padded;
        // the declared size wins and the rest is ignored.
        assert!(encode_png(1, 1, &[1; 64]).is_ok());
    }
}
