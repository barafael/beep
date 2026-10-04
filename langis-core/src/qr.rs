use qrcode::{Color, EcLevel, QrCode};

/// A rendered QR code as a square grid of dark/light modules, toolkit-independent.
#[derive(Clone, Debug, PartialEq)]
pub struct QrMatrix {
    pub width: usize,
    /// Row-major, `true` = dark module.
    pub modules: Vec<bool>,
}

impl QrMatrix {
    /// Encodes `data`, or `None` when it does not fit in a QR code at this error correction
    /// level. Callers that show the code to a user should report that rather than leave a blank
    /// panel, since there is nothing else to explain the failure.
    pub fn encode(data: &str) -> Option<Self> {
        // Level L, not M: the only thing encoded here is a provisioning URI, which is a bearer
        // credential that links this device to the account. It is shown on screen for as long as
        // the phone takes to scan it, and the extra redundancy means it survives being
        // photographed at an angle or through a glare.
        let code = QrCode::with_error_correction_level(data, EcLevel::L).ok()?;
        Some(Self {
            width: code.width(),
            modules: code.to_colors().into_iter().map(|c| c == Color::Dark).collect(),
        })
    }

    /// RGBA8 pixels with `scale` pixels per module and a 4-module quiet zone.
    pub fn to_rgba(&self, scale: usize) -> (usize, Vec<u8>) {
        const QUIET: usize = 4;
        let side = (self.width + 2 * QUIET) * scale;
        let mut pixels = vec![0xff; side * side * 4];
        for (i, _) in self.modules.iter().enumerate().filter(|(_, dark)| **dark) {
            let (mx, my) = (i % self.width + QUIET, i / self.width + QUIET);
            for y in my * scale..(my + 1) * scale {
                let row = y * side * 4;
                let span = &mut pixels[row + mx * scale * 4..row + (mx + 1) * scale * 4];
                span.as_chunks_mut::<4>().0.fill([0, 0, 0, 0xff]);
            }
        }
        (side, pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_renders() {
        let qr = QrMatrix::encode("sgnl://linkdevice?uuid=test").unwrap();
        assert!(qr.width > 0);
        assert_eq!(qr.modules.len(), qr.width * qr.width);
        let (side, pixels) = qr.to_rgba(2);
        // Four quiet modules on each side, and the pixels are RGBA.
        assert_eq!(side, (qr.width + 8) * 2);
        assert_eq!(pixels.len(), side * side * 4);
        assert!(pixels.chunks_exact(4).any(|p| p == [0, 0, 0, 0xff]), "no dark modules");
        assert!(pixels.chunks_exact(4).any(|p| p == [0xff, 0xff, 0xff, 0xff]), "no light modules");
    }

    /// The quiet zone is what lets a scanner find the code, so it must never be trimmed.
    #[test]
    fn has_a_quiet_zone() {
        let qr = QrMatrix::encode("sgnl://linkdevice?uuid=test").unwrap();
        let (side, pixels) = qr.to_rgba(1);
        let light = |x: usize, y: usize| {
            let i = (y * side + x) * 4;
            pixels[i..i + 4] == [0xff, 0xff, 0xff, 0xff]
        };
        for offset in 0..4 {
            assert!(light(offset, 0));
            assert!(light(0, offset));
            assert!(light(side - 1 - offset, 0));
            assert!(light(0, side - 1 - offset));
        }
    }

    #[test]
    fn refuses_data_that_does_not_fit() {
        assert!(QrMatrix::encode(&"x".repeat(10_000)).is_none());
    }
}
