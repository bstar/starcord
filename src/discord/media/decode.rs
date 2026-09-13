//! Turning bytes somebody else produced into pixels.
//!
//! Everything here is written on the assumption that the bytes are hostile,
//! because they arrived over a socket from a stranger's computer.
//!
//! - **The header is read before the image is.** A four-hundred-byte PNG can
//!   declare itself forty thousand pixels on a side, and a decoder that
//!   believes it allocates six gigabytes before it discovers there is no data
//!   behind the claim. Dimensions are read first, checked, and only then is
//!   anything decoded — with the decoder's own allocation limit set as well,
//!   because the dimensions are not the only number in a file.
//! - **An animation is capped twice.** Three hundred frames or fifty million
//!   pixels in total, whichever comes first. Past either, the first frame is
//!   kept as a still and the caller is told why. A GIF that is thirty seconds
//!   of full-screen video is a legal GIF.
//! - **Frame delays have a floor.** A GIF with a zero delay is asking to be
//!   drawn as fast as the machine can manage, which in a terminal means
//!   redrawing the whole screen a thousand times a second. Twenty milliseconds
//!   is the same floor browsers apply.
//!
//! None of this is async. Decoding is the one genuinely CPU-bound thing the
//! core does, and it runs on a blocking thread — see `fetch.rs`.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use image::imageops::FilterType;
use image::{AnimationDecoder, ImageFormat, RgbaImage};

use super::{Decoded, MediaError};

/// The largest picture worth decoding, on a side.
///
/// The same number STAR/KIT uses: far above anything Discord will serve, and
/// far enough above a phone photograph that nothing legitimate is refused.
pub const MAX_DIMENSION: u32 = 8192;

/// How much a single decode may allocate.
///
/// 8192 squared at four bytes a pixel is 256 MiB, so this is deliberately below
/// what the dimension limit alone would permit: the two together mean a picture
/// has to be both plausibly sized and plausibly large.
pub const MAX_ALLOC: u64 = 128 * 1024 * 1024;

/// The most frames kept from one animation.
pub const MAX_FRAMES: usize = 300;

/// The most pixels kept from one animation, across every frame.
pub const MAX_ANIMATION_PIXELS: u64 = 50_000_000;

/// The shortest frame delay that will be honoured.
pub const MIN_DELAY: Duration = Duration::from_millis(20);

/// What came out, and anything the user should be told about it.
#[derive(Debug)]
pub struct Decoding {
    pub decoded: Decoded,
    /// Set when the picture was shown differently from how it was sent — an
    /// animation too long to keep, so far the only case. A note rather than an
    /// error: the user gets the picture, and an explanation of why it is not
    /// moving.
    pub note: Option<String>,
}

/// Decode `bytes`, fitting the result inside `max_w` by `max_h`.
///
/// A zero in either bound means "do not resize", which is what a save-to-disk
/// or an external viewer wants.
pub fn decode(bytes: &[u8], max_w: u32, max_h: u32) -> Result<Decoding, MediaError> {
    let format = image::guess_format(bytes).map_err(|e| {
        MediaError::Unsupported(format!("the format is not one this client reads ({e})"))
    })?;

    let (width, height) = dimensions(bytes)?;
    check(width, height)?;

    match format {
        ImageFormat::Gif => animated(bytes, max_w, max_h, format),
        ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))
                .map_err(|e| MediaError::Decode(e.to_string()))?;
            if decoder.has_animation() {
                animated(bytes, max_w, max_h, format)
            } else {
                still(bytes, max_w, max_h)
            }
        }
        _ => still(bytes, max_w, max_h),
    }
}

/// Read the size out of the header without decoding anything.
pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32), MediaError> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| MediaError::Decode(e.to_string()))?
        .into_dimensions()
        .map_err(|e| MediaError::Decode(e.to_string()))
}

/// Whether a picture of this size is worth the memory.
fn check(width: u32, height: u32) -> Result<(), MediaError> {
    if width == 0 || height == 0 {
        return Err(MediaError::Decode("it has no pixels".into()));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(MediaError::TooLarge {
            limit: u64::from(MAX_DIMENSION),
        });
    }
    let bytes = u64::from(width) * u64::from(height) * 4;
    if bytes > MAX_ALLOC {
        return Err(MediaError::TooLarge { limit: MAX_ALLOC });
    }
    Ok(())
}

/// The decoder limits, applied on top of the header check.
///
/// Both halves are needed. The header check rejects a declared size; these
/// reject a file whose declared size was fine and whose contents were not.
fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOC);
    limits
}

fn still(bytes: &[u8], max_w: u32, max_h: u32) -> Result<Decoding, MediaError> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| MediaError::Decode(e.to_string()))?;
    reader.limits(limits());
    let image = reader
        .decode()
        .map_err(|e| MediaError::Decode(e.to_string()))?;
    let image = fit(image.into_rgba8(), max_w, max_h);
    Ok(Decoding {
        decoded: Decoded::Still(Arc::new(image)),
        note: None,
    })
}

fn animated(
    bytes: &[u8],
    max_w: u32,
    max_h: u32,
    format: ImageFormat,
) -> Result<Decoding, MediaError> {
    let frames = match format {
        ImageFormat::Gif => {
            let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
                .map_err(|e| MediaError::Decode(e.to_string()))?;
            collect(decoder.into_frames())
        }
        _ => {
            let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))
                .map_err(|e| MediaError::Decode(e.to_string()))?;
            collect(decoder.into_frames())
        }
    }?;

    let Collected {
        mut frames,
        mut delays,
        truncated,
    } = frames;

    if frames.is_empty() {
        return Err(MediaError::Decode("the animation had no frames".into()));
    }

    // Past either cap the picture is still shown, as its first frame. A still
    // picture is a worse answer than an animation and a much better one than a
    // client that has eaten a gigabyte of memory.
    if truncated {
        let first = fit(frames.remove(0), max_w, max_h);
        return Ok(Decoding {
            decoded: Decoded::Still(Arc::new(first)),
            note: Some(format!(
                "an animation longer than {MAX_FRAMES} frames is shown as its first frame"
            )),
        });
    }

    let frames: Vec<Arc<RgbaImage>> = frames
        .into_iter()
        .map(|frame| Arc::new(fit(frame, max_w, max_h)))
        .collect();
    delays.truncate(frames.len());

    Ok(Decoding {
        decoded: Decoded::Animated {
            frames,
            delays,
            // Neither the GIF nor the WebP decoder exposes the loop count, and
            // an animation nobody meant to repeat is vanishingly rare beside
            // one that was. The UI's animation policy decides how often it
            // actually plays.
            looped: true,
        },
        note: None,
    })
}

struct Collected {
    frames: Vec<RgbaImage>,
    delays: Vec<Duration>,
    truncated: bool,
}

fn collect(frames: image::Frames<'_>) -> Result<Collected, MediaError> {
    let mut images = Vec::new();
    let mut delays = Vec::new();
    let mut pixels: u64 = 0;
    let mut truncated = false;

    for frame in frames {
        let frame = frame.map_err(|e| MediaError::Decode(e.to_string()))?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let millis = if denominator == 0 {
            0
        } else {
            u64::from(numerator) / u64::from(denominator).max(1)
        };
        let buffer = frame.into_buffer();

        pixels += u64::from(buffer.width()) * u64::from(buffer.height());
        images.push(buffer);
        delays.push(Duration::from_millis(millis).max(MIN_DELAY));

        if images.len() > MAX_FRAMES || pixels > MAX_ANIMATION_PIXELS {
            truncated = true;
            break;
        }
    }

    Ok(Collected {
        frames: images,
        delays,
        truncated,
    })
}

/// Scale down to fit, never up.
///
/// `Triangle` rather than the sharper filters: the result is going into cells a
/// few pixels across, where the difference is invisible and the cost is not.
fn fit(image: RgbaImage, max_w: u32, max_h: u32) -> RgbaImage {
    if max_w == 0 || max_h == 0 {
        return image;
    }
    let (width, height) = image.dimensions();
    if width <= max_w && height <= max_h {
        return image;
    }
    let scale = f64::from(max_w) / f64::from(width);
    let scale = scale.min(f64::from(max_h) / f64::from(height));
    let target_w = ((f64::from(width) * scale).round() as u32).max(1);
    let target_h = ((f64::from(height) * scale).round() as u32).max(1);
    image::imageops::resize(&image, target_w, target_h, FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Frame};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let mut out = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    fn gif(frames: usize, width: u32, height: u32, delay_ms: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut out);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .unwrap();
            let built: Vec<Frame> = (0..frames)
                .map(|n| {
                    let shade = (n * 40 % 256) as u8;
                    let buffer =
                        RgbaImage::from_pixel(width, height, image::Rgba([shade, 40, 200, 255]));
                    Frame::from_parts(buffer, 0, 0, Delay::from_numer_denom_ms(delay_ms, 1))
                })
                .collect();
            encoder.encode_frames(built).unwrap();
        }
        out
    }

    #[test]
    fn a_still_picture_decodes_to_its_own_size() {
        let decoding = decode(&png(40, 20), 0, 0).unwrap();
        assert_eq!(decoding.decoded.dimensions(), Some((40, 20)));
        assert_eq!(decoding.decoded.frame_count(), 1);
        assert!(decoding.note.is_none());
    }

    #[test]
    fn a_picture_larger_than_the_bounds_is_scaled_and_keeps_its_shape() {
        let decoding = decode(&png(400, 200), 100, 100).unwrap();
        assert_eq!(
            decoding.decoded.dimensions(),
            Some((100, 50)),
            "the aspect ratio has to survive"
        );

        // Never scaled up.
        let small = decode(&png(10, 10), 100, 100).unwrap();
        assert_eq!(small.decoded.dimensions(), Some((10, 10)));
    }

    /// The whole reason the header is read separately.
    #[test]
    fn a_small_file_claiming_to_be_enormous_is_refused_before_it_is_decoded() {
        let bytes = png_header(40_000, 40_000);
        assert_eq!(dimensions(&bytes).unwrap(), (40_000, 40_000));
        assert!(
            matches!(decode(&bytes, 64, 64), Err(MediaError::TooLarge { .. })),
            "a picture larger than the limit was not refused"
        );

        // Within the dimension limit, over the allocation limit: 8000 by 8000
        // at four bytes a pixel is 256 MiB.
        let wide = png_header(8000, 8000);
        assert!(matches!(
            decode(&wide, 64, 64),
            Err(MediaError::TooLarge { .. })
        ));
    }

    #[test]
    fn a_gif_decodes_to_frames_with_delays() {
        let decoding = decode(&gif(4, 16, 16, 100), 0, 0).unwrap();
        match &decoding.decoded {
            Decoded::Animated {
                frames,
                delays,
                looped,
            } => {
                assert_eq!(frames.len(), 4);
                assert_eq!(delays.len(), 4);
                assert!(delays.iter().all(|d| *d == Duration::from_millis(100)));
                assert!(looped);
                assert_eq!(frames[0].dimensions(), (16, 16));
            }
            other => panic!("a four-frame gif decoded as {other:?}"),
        }
        assert!(decoding.note.is_none());
    }

    /// A zero delay means "as fast as you can", which in a terminal means
    /// redrawing the screen a thousand times a second.
    #[test]
    fn a_frame_delay_has_a_floor() {
        let decoding = decode(&gif(3, 8, 8, 0), 0, 0).unwrap();
        match &decoding.decoded {
            Decoded::Animated { delays, .. } => {
                assert!(delays.iter().all(|d| *d >= MIN_DELAY), "{delays:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A GIF that is really a video is shown rather than refused, and the user
    /// is told why it is not moving.
    #[test]
    fn an_animation_past_the_cap_becomes_its_first_frame() {
        // Fifty million pixels is the other cap; this crosses the frame count.
        let bytes = gif(MAX_FRAMES + 5, 4, 4, 40);
        let decoding = decode(&bytes, 0, 0).unwrap();
        assert!(
            matches!(decoding.decoded, Decoded::Still(_)),
            "{:?}",
            decoding.decoded
        );
        assert_eq!(decoding.decoded.dimensions(), Some((4, 4)));
        let note = decoding.note.expect("nothing said why it stopped moving");
        assert!(note.contains("first frame"), "{note}");
    }

    #[test]
    fn rubbish_is_an_error_rather_than_a_panic() {
        for bytes in [
            &b""[..],
            &b"not an image"[..],
            &b"\x89PNG\r\n\x1a\n"[..],
            &b"GIF89a"[..],
            &[0xff; 512][..],
        ] {
            let result = decode(bytes, 64, 64);
            assert!(result.is_err(), "{bytes:?} decoded to something");
        }

        // A real header followed by nothing.
        let mut truncated = png(8, 8);
        truncated.truncate(30);
        assert!(decode(&truncated, 64, 64).is_err());
    }

    /// The bound the whole module is built around: bytes from a stranger may
    /// produce any error and no panic.
    #[test]
    fn arbitrary_bytes_never_panic() {
        use proptest::prelude::*;

        proptest!(ProptestConfig::with_cases(128), |(bytes in proptest::collection::vec(any::<u8>(), 0..4096))| {
            let _ = decode(&bytes, 32, 32);
            let _ = dimensions(&bytes);
        });
    }

    /// Any prefix of a real picture, which is what a truncated download is.
    #[test]
    fn a_truncated_picture_never_panics() {
        use proptest::prelude::*;

        let whole = png(24, 18);
        let animation = gif(3, 8, 8, 60);
        proptest!(ProptestConfig::with_cases(64), |(cut in 0usize..200)| {
            let _ = decode(&whole[..cut.min(whole.len())], 16, 16);
            let _ = decode(&animation[..cut.min(animation.len())], 16, 16);
        });
    }

    /// A PNG header and nothing else: thirty-three bytes that declare a size.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"IHDR");
        chunk.extend_from_slice(&width.to_be_bytes());
        chunk.extend_from_slice(&height.to_be_bytes());
        chunk.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&crc32(&chunk).to_be_bytes());

        // The reader walks chunks until it reaches the image data, so there has
        // to be one for it to stop at. It is empty: the point of the fixture is
        // that the size is a claim with nothing behind it.
        let idat = b"IDAT".to_vec();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&idat);
        out.extend_from_slice(&crc32(&idat).to_be_bytes());
        out
    }

    /// The PNG decoder checks chunk checksums, so the fixture has to carry a
    /// real one.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = 0u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }
}
