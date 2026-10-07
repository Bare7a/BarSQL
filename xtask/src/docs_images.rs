// Landing page images from the README screenshots and the app icon. The page picks a scaled copy to fit the
// screen; the lightbox opens a lossless WebP at the screenshot's native size.
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops;
use image::{ExtendedColorType, RgbImage};

// A phone's 2x screen needs 800 and a 1x laptop's hero 1280, where 640 alone sent the full-size image instead.
const WIDTHS: [u32; 3] = [640, 800, 1280];
// Link preview, cropped from the top of the first screenshot.
const PREVIEW: (u32, u32) = (1200, 630);

pub fn run(root: &Path) -> Result<(), String> {
    let (from, docs) = (root.join(".github/screenshots"), root.join("docs"));
    let to = docs.join("screenshots");
    if to.exists() {
        fs::remove_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    fs::create_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut shots: Vec<PathBuf> = fs::read_dir(&from)
        .map_err(|e| format!("{}: {e}", from.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .collect();
    if shots.is_empty() {
        return Err(format!("no screenshots in {}", from.display()));
    }
    shots.sort();
    for path in &shots {
        let shot = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?.into_rgb8();
        let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
        let original = to.join(format!("{name}.webp"));
        write_webp(&original, &shot)?;
        report(&original);
        for width in WIDTHS {
            let scaled = to.join(format!("{name}-{width}.webp"));
            write_webp(&scaled, &scale(&shot, width))?;
            report(&scaled);
        }
        if name == "1" {
            let preview = scale(&shot, PREVIEW.0);
            let preview = imageops::crop_imm(&preview, 0, 0, PREVIEW.0, PREVIEW.1).to_image();
            let out = docs.join("preview.jpg");
            JpegEncoder::new_with_quality(create(&out)?, 88)
                .encode_image(&preview)
                .map_err(|e| format!("{}: {e}", out.display()))?;
            report(&out);
        }
    }
    for (icon, copy) in [("packaging/icon.svg", "icon.svg"), ("packaging/linux/barsql-256.png", "icon.png")] {
        let out = docs.join(copy);
        fs::copy(root.join(icon), &out).map_err(|e| format!("{icon}: {e}"))?;
        report(&out);
    }
    Ok(())
}

fn write_webp(path: &Path, image: &RgbImage) -> Result<(), String> {
    WebPEncoder::new_lossless(create(path)?)
        .encode(image.as_raw(), image.width(), image.height(), ExtendedColorType::Rgb8)
        .map_err(|e| format!("{}: {e}", path.display()))
}

// Averages whole pixel areas, which is fast and sharp enough at the sizes used here.
fn scale(image: &RgbImage, width: u32) -> RgbImage {
    let height = (u64::from(image.height()) * u64::from(width) / u64::from(image.width())) as u32;
    imageops::thumbnail(image, width, height)
}

fn create(path: &Path) -> Result<BufWriter<File>, String> {
    File::create(path).map(BufWriter::new).map_err(|e| format!("{}: {e}", path.display()))
}

fn report(path: &Path) {
    let size = fs::metadata(path).map(|meta| meta.len()).unwrap_or_default();
    println!("{} ({} KB)", path.display(), size.div_ceil(1024));
}
