//! Pictures made ready to send as Signal's apps make them: no larger than they need to be, so
//! that every receiver, and every linked device of ours, downloads and decodes a photo of about
//! 1 MiB rather than the camera's 5 to 10. The levels are Signal's: Signal-Desktop
//! `ts/util/scaleImageToLevel.preload.ts`, Signal-Android `PushMediaConstraints.java`.
//!
//! Each step down in size is tried until the picture fits its level's bytes; the smallest step
//! goes out whatever it weighs. A small picture is only drawn again, as before: its orientation
//! applied and its metadata (where it was taken, the camera) left out. Only still JPEG and PNG
//! are touched; anything else (GIF, WebP, a format b33p can't decode) goes as it is.

use crate::Picture;

/// How far a level lets a picture go.
struct Level {
    /// Longest edge, in pixels.
    edge: u32,
    /// What a picture may weigh before it is scaled down.
    bytes: usize,
    /// Under this, and within `edge`, a picture is only drawn again.
    small: usize,
    jpeg_quality: u8,
    /// The longest edges tried, in order, until the picture weighs at most `bytes`.
    steps: &'static [u32],
}

/// Signal's default: LEVEL_1 on Android, level 1 on Desktop.
const STANDARD: Level = Level {
    edge: 1600,
    bytes: 1024 * 1024,
    small: 200 * 1024,
    jpeg_quality: 70,
    steps: &[1600, 1024, 768, 512],
};

/// Signal's "high quality": LEVEL_3 on Android, level 3 on Desktop.
const HIGH: Level = Level {
    edge: 4096,
    bytes: 3 * 1024 * 1024,
    small: 400 * 1024,
    jpeg_quality: 75,
    steps: &[4096, 3072, 2048, 1600, 1024, 768, 512],
};

/// The quality pictures are drawn again at when they aren't scaled: as good as they came.
const REDRAWN_QUALITY: u8 = 90;

/// A picture as it goes out: its type (a PNG without transparency goes as JPEG), its bytes,
/// and the blurred stand-in receivers draw until they have it, as Signal's apps send one.
#[derive(Debug)]
pub(crate) struct Ready {
    pub(crate) content_type: String,
    pub(crate) data: Vec<u8>,
    pub(crate) blur_hash: Option<String>,
}

/// The longest edge a picture is shrunk to for its blurhash: the hash keeps 4 by 3 colours,
/// and Signal-Android computes it from a picture 16 times smaller.
const BLUR_EDGE: u32 = 64;

/// `data`, of `content_type`, made ready to send at Signal's standard level or, with `high`, its
/// high-quality one.
pub(crate) fn for_sending(content_type: &str, data: Vec<u8>, high: bool) -> Ready {
    let level = if high { &HIGH } else { &STANDARD };
    let format = match content_type {
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/png" => image::ImageFormat::Png,
        _ => return as_it_is(content_type, data),
    };
    match scale(&data, format, level) {
        Ok(ready) => ready,
        Err(error) => {
            tracing::warn!(error = %crate::scrub::chain(&error), "couldn't make a picture ready to send; sending it as it is");
            as_it_is(content_type, data)
        }
    }
}

fn as_it_is(content_type: &str, data: Vec<u8>) -> Ready {
    Ready {
        content_type: content_type.to_owned(),
        data,
        blur_hash: None,
    }
}

/// `picture`'s blurhash, 4 by 3 components as Signal's apps make it.
fn blur_hash(picture: &Picture) -> Option<String> {
    let edge = picture.width.max(picture.height);
    let small = (edge > BLUR_EDGE)
        .then(|| {
            let (width, height) = crate::resample::scaled(
                (picture.width, picture.height),
                f64::from(BLUR_EDGE) / f64::from(edge),
            );
            crate::resample::shrink(picture, width, height)
        })
        .flatten();
    let small = small.as_ref().unwrap_or(picture);
    blurhash::encode(4, 3, small.width, small.height, &small.rgba).ok()
}

fn scale(data: &[u8], format: image::ImageFormat, level: &Level) -> image::ImageResult<Ready> {
    use image::ImageDecoder as _;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(data), format);
    reader.limits(limits());
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut decoded = image::DynamicImage::from_decoder(decoder)?;
    decoded.apply_orientation(orientation);
    let rgba = decoded.into_rgba8();
    let picture = Picture {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    };
    // Transparency would turn black in a JPEG.
    let opaque = picture
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[3] == u8::MAX);
    // Once, from the picture as it came: the steps below only make it smaller.
    let blurred = blur_hash(&picture);
    let with_hash = |mut ready: Ready| {
        ready.blur_hash.clone_from(&blurred);
        ready
    };
    let edge = picture.width.max(picture.height);
    if data.len() <= level.small && edge <= level.edge {
        return encode(&picture, opaque, REDRAWN_QUALITY).map(with_hash);
    }
    let mut last = None;
    for &step in level.steps {
        let scaled = if edge > step {
            let (width, height) = crate::resample::scaled(
                (picture.width, picture.height),
                f64::from(step) / f64::from(edge),
            );
            crate::resample::shrink(&picture, width, height)
        } else {
            None
        };
        let ready = encode(
            scaled.as_ref().unwrap_or(&picture),
            opaque,
            level.jpeg_quality,
        )?;
        if ready.data.len() <= level.bytes {
            return Ok(with_hash(ready));
        }
        last = Some(ready);
    }
    Ok(with_hash(last.expect("a level has steps")))
}

/// `picture` as JPEG at `quality` if `opaque`, else as PNG.
fn encode(picture: &Picture, opaque: bool, quality: u8) -> image::ImageResult<Ready> {
    use image::ImageEncoder as _;
    let mut out = Vec::new();
    if opaque {
        let rgb: Vec<u8> = picture
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).write_image(
            &rgb,
            picture.width,
            picture.height,
            image::ExtendedColorType::Rgb8,
        )?;
        Ok(Ready {
            content_type: "image/jpeg".into(),
            data: out,
            blur_hash: None,
        })
    } else {
        image::codecs::png::PngEncoder::new(&mut out).write_image(
            &picture.rgba,
            picture.width,
            picture.height,
            image::ExtendedColorType::Rgba8,
        )?;
        Ok(Ready {
            content_type: "image/png".into(),
            data: out,
            blur_hash: None,
        })
    }
}

/// As for pictures received ([`super::decode_limited`]): a file can claim any size.
fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(512 * 1024 * 1024);
    limits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera-sized photo that doesn't compress away: noise, as a sensor's.
    fn photo(width: u32, height: u32) -> Vec<u8> {
        let mut state: u32 = 12345;
        let picture = image::RgbImage::from_fn(width, height, |x, y| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let noise = (state >> 24) as u8 / 2;
            image::Rgb([
                (x * 255 / width) as u8 / 2 + noise,
                (y * 255 / height) as u8 / 2 + noise,
                noise,
            ])
        });
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95)
            .encode_image(&picture)
            .unwrap();
        out
    }

    fn size(ready: &Ready) -> (u32, u32) {
        image::load_from_memory(&ready.data)
            .unwrap()
            .into_rgb8()
            .dimensions()
    }

    #[test]
    fn a_camera_photo_goes_out_as_signals_apps_send_it() {
        let original = photo(4032, 3024);
        assert!(original.len() > 3 * 1024 * 1024, "{}", original.len());

        let standard = for_sending("image/jpeg", original.clone(), false);
        assert_eq!(standard.content_type, "image/jpeg");
        assert!(
            standard.data.len() <= STANDARD.bytes,
            "{}",
            standard.data.len()
        );
        let (width, height) = size(&standard);
        assert!(
            width.max(height) <= 1600 && width * 3 == height * 4,
            "{width}x{height}"
        );

        // With its blurred stand-in, as receivers decode it: the photo's colours, greener below.
        let hash = standard.blur_hash.as_deref().expect("a blurhash");
        let blurred = blurhash::decode(hash, 2, 2, 1.0).unwrap();
        let (top, bottom) = (&blurred[0..4], &blurred[8..12]);
        assert!(bottom[1] > top[1], "{hash}: {top:?} above {bottom:?}");

        let high = for_sending("image/jpeg", original, true);
        assert!(high.data.len() <= HIGH.bytes, "{}", high.data.len());
        assert!(size(&high).0 > width, "high quality keeps more of it");
    }

    #[test]
    fn a_small_picture_keeps_its_size() {
        let small = photo(320, 240);
        assert!(small.len() < STANDARD.small);
        assert_eq!(size(&for_sending("image/jpeg", small, false)), (320, 240));
    }

    #[test]
    fn a_png_goes_as_jpeg_unless_it_is_see_through() {
        let png = |alpha: u8| {
            let mut out = Vec::new();
            image::RgbaImage::from_pixel(64, 64, image::Rgba([10, 200, 30, alpha]))
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        assert_eq!(
            for_sending("image/png", png(255), false).content_type,
            "image/jpeg"
        );
        let see_through = for_sending("image/png", png(100), false);
        assert_eq!(see_through.content_type, "image/png");
        let pixel = image::load_from_memory(&see_through.data)
            .unwrap()
            .into_rgba8()
            .get_pixel(3, 3)
            .0;
        assert_eq!(pixel, [10, 200, 30, 100]);
    }

    #[test]
    fn what_isnt_a_still_jpeg_or_png_goes_as_it_is() {
        let gif = b"GIF89a not decoded".to_vec();
        let ready = for_sending("image/gif", gif.clone(), false);
        assert_eq!(ready.blur_hash, None);
        assert_eq!(
            (ready.content_type.as_str(), ready.data),
            ("image/gif", gif)
        );
        let broken = b"not a jpeg".to_vec();
        assert_eq!(
            for_sending("image/jpeg", broken.clone(), false).data,
            broken
        );
    }
}
