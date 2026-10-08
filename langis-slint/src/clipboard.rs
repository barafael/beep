//! Pictures and files pasted into the composer. A pasted picture becomes a PNG file in a
//! private directory, so it can be staged like any other file; the directory is emptied when
//! langis starts.

use std::path::PathBuf;

/// What the clipboard holds to attach: copied files, or a picture (saved as a file). `None`
/// if it holds neither, e.g. just text, which the composer pastes itself.
pub fn pasted() -> Option<Vec<PathBuf>> {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(error) => {
            tracing::debug!(%error, "no clipboard");
            return None;
        }
    };
    if let Ok(files) = clipboard.get().file_list()
        && !files.is_empty()
    {
        return Some(files);
    }
    let image = clipboard.get_image().ok()?;
    match save_png(&image) {
        Ok(path) => Some(vec![path]),
        Err(error) => {
            tracing::warn!(%error, "couldn't keep a pasted picture");
            None
        }
    }
}

/// Where pasted pictures are kept until sent; see `langis_core::scratch`.
const PASTED: &str = "langis-pasted";

/// Forgets pasted pictures: an earlier run's at the start, this run's at the end.
pub fn clean() {
    langis_core::scratch::clean(PASTED);
}

fn save_png(image: &arboard::ImageData) -> std::io::Result<PathBuf> {
    let dir = langis_core::scratch::dir(PASTED)?;
    let name = format!("pasted-{}.png", langis_core::now_millis());
    let path = dir.join(name);
    let file = std::fs::File::create(&path)?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        image.width as u32,
        image.height as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&image.bytes))
        .map_err(std::io::Error::other)?;
    Ok(path)
}
