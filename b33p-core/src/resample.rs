//! Scaling pictures down well, for the picture viewer: Lanczos3, which keeps a photo sharp
//! without the stairs and moiré of drawing it smaller with bilinear filtering (what the window
//! does), in premultiplied alpha so see-through pixels' colours don't bleed into the edges.
//! With `fast_image_resize`: integer SIMD, much faster than `image`'s resize and without its
//! float buffers. Ported in idea from Othereum/sigwin's `resize_into` (88ea8a5).

use crate::Picture;

/// `picture` scaled down to `width` × `height`; `None` unless that is smaller (on one side at
/// least, and larger on neither) and not empty.
///
/// Always the whole picture, never a crop, and never larger: `fast_image_resize` (6.1) can read
/// out of bounds when it enlarges from a crop with a kernel that is zero at whole numbers
/// (Cykooz/fast_image_resize#63), which this can't ask for.
pub fn shrink(picture: &Picture, width: u32, height: u32) -> Option<Picture> {
    use fast_image_resize::images::{Image, ImageRef};
    use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
    let smaller = width < picture.width || height < picture.height;
    let larger = width > picture.width || height > picture.height;
    if !smaller || larger || width == 0 || height == 0 {
        return None;
    }
    let source = ImageRef::new(
        picture.width,
        picture.height,
        &picture.rgba,
        PixelType::U8x4,
    )
    .ok()?;
    let mut rgba = vec![0; width as usize * height as usize * 4];
    let mut target = Image::from_slice_u8(width, height, &mut rgba, PixelType::U8x4).ok()?;
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
        .use_alpha(true);
    Resizer::new().resize(&source, &mut target, &options).ok()?;
    Some(Picture {
        width,
        height,
        rgba,
    })
}

/// `width` × `height` scaled by `scale`, rounded, at least a pixel a side.
pub fn scaled((width, height): (u32, u32), scale: f64) -> (u32, u32) {
    let side = |side: u32| ((f64::from(side) * scale).round() as u32).max(1);
    (side(width), side(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: u32, height: u32, pixel: [u8; 4]) -> Picture {
        Picture {
            width,
            height,
            rgba: pixel.repeat(width as usize * height as usize),
        }
    }

    /// Lands on the size asked for, whatever the shape; only ever smaller.
    #[test]
    fn shrinks_to_every_size_and_never_grows() {
        for (width, height) in [(37, 1), (1, 37), (101, 57), (257, 256), (3, 300)] {
            let picture = flat(width, height, [0; 4]);
            for edge in 1..width.max(height) {
                let (w, h) = scaled(
                    (width, height),
                    f64::from(edge) / f64::from(width.max(height)),
                );
                let shrunk = shrink(&picture, w, h).unwrap();
                assert_eq!((shrunk.width, shrunk.height), (w, h));
                assert_eq!(shrunk.rgba.len(), (w * h * 4) as usize);
            }
            assert!(shrink(&picture, width, height).is_none(), "the same size");
            assert!(shrink(&picture, width + 1, height).is_none(), "larger");
            assert!(shrink(&picture, 0, 0).is_none());
        }
    }

    /// A flat colour stays that colour, see-through or not (give or take the rounding of
    /// premultiplying): no fringes.
    #[test]
    fn keeps_flat_colours() {
        for pixel in [[200, 100, 50, 255], [200, 100, 50, 128]] {
            let shrunk = shrink(&flat(300, 200, pixel), 97, 61).unwrap();
            assert!(
                shrunk
                    .rgba
                    .chunks(4)
                    .all(|p| p.iter().zip(pixel).all(|(&a, b)| a.abs_diff(b) <= 1)),
                "{pixel:?}"
            );
        }
    }

    /// Stripes finer than the new pixels average out to an even tone, where drawing smaller
    /// with bilinear filtering leaves a moiré: why the viewer resamples at all.
    #[test]
    fn fine_stripes_average_out() {
        let (width, height) = (400, 40);
        let rgba = (0..width * height)
            .flat_map(|i| {
                let v = if i % width % 2 == 0 { 0 } else { 200 };
                [v, v, v, 255]
            })
            .collect();
        let picture = Picture {
            width,
            height,
            rgba,
        };
        let shrunk = shrink(&picture, 184, 18).unwrap();
        let tones: Vec<u8> = shrunk.rgba.chunks(4).map(|p| p[0]).collect();
        let inner = &tones[4..180];
        let (low, high) = (inner.iter().min().unwrap(), inner.iter().max().unwrap());
        assert!(high - low < 10, "from {low} to {high}");
    }
}
