use qrcode::{Color, EcLevel, QrCode};

/// A rendered QR code as a square grid of dark/light modules, toolkit-independent.
#[derive(Clone, Debug, PartialEq)]
pub struct QrMatrix {
    pub width: usize,
    /// Row-major, `true` = dark module.
    pub modules: Vec<bool>,
}

impl QrMatrix {
    pub fn encode(data: &str) -> Option<Self> {
        let code = QrCode::with_error_correction_level(data, EcLevel::M).ok()?;
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
