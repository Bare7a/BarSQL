// The README's and landing page's images, from the screenshots kept in docs/screenshots. The icons command writes
// the app icon's copies.
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::{self, FilterType};
use image::{DynamicImage, ExtendedColorType, Rgb, RgbImage};

use crate::report;

// A phone's 2x screen needs 800 and a 1x laptop's hero 1280, where 640 alone sent the full-size image instead.
const WIDTHS: [u32; 3] = [640, 800, 1280];
// Link preview, cropped from the top of the first screenshot.
const PREVIEW: (u32, u32) = (1200, 630);
// The play button's radius, as a share of the poster's width.
const PLAY: f32 = 0.045;

// The app's PNGs in `shots` replace the kept screenshots, losslessly.
pub fn keep(shots: &Path, root: &Path) -> Result<(), String> {
    let to = root.join("docs/screenshots");
    if to.exists() {
        fs::remove_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    fs::create_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
    for path in files(shots, "png")? {
        let shot = image::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
        let kept = to.join(format!("{name}.webp"));
        write_webp(&kept, &shot.to_rgb8())?;
        report(&kept);
    }
    Ok(())
}

pub fn run(root: &Path) -> Result<(), String> {
    let docs = root.join("docs");
    let dir = docs.join("screenshots");
    // Not the copies written beside them.
    let shots: Vec<PathBuf> = files(&dir, "webp")?
        .into_iter()
        .filter(|path| path.file_stem().and_then(|stem| stem.to_str()).is_some_and(|stem| !stem.contains('-')))
        .collect();
    if shots.is_empty() {
        return Err(format!("no screenshots in {}", dir.display()));
    }
    for path in &shots {
        let shot = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
        for width in WIDTHS {
            let scaled = dir.join(format!("{name}-{width}.webp"));
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
            let out = docs.join("demo.webp");
            write_webp(&out, &poster(&shot))?;
            report(&out);
        }
    }
    Ok(())
}

fn files(dir: &Path, extension: &str) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .collect();
    files.sort();
    Ok(files)
}

fn write_webp(path: &Path, image: &RgbImage) -> Result<(), String> {
    WebPEncoder::new_lossless(create(path)?)
        .encode(image.as_raw(), image.width(), image.height(), ExtendedColorType::Rgb8)
        .map_err(|e| format!("{}: {e}", path.display()))
}

// For the README's link to the demo video, which GitHub can't play from the repository.
fn poster(shot: &DynamicImage) -> RgbImage {
    let mut poster = shot.to_rgb8();
    let (cx, cy) = (poster.width() as f32 / 2., poster.height() as f32 / 2.);
    let radius = poster.width() as f32 * PLAY;
    let rim = radius * 0.06;
    // A little right of the middle, where it looks centred.
    let arrow =
        [(cx - radius * 0.22, cy - radius * 0.42), (cx + radius * 0.51, cy), (cx - radius * 0.22, cy + radius * 0.42)];
    let span = |middle: f32| (middle - radius - 1.).max(0.) as u32..(middle + radius + 2.) as u32;
    for y in span(cy) {
        for x in span(cx) {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let off = (px - cx).hypot(py - cy);
            let disc = (radius + 0.5 - off).clamp(0., 1.);
            if disc == 0. {
                continue;
            }
            let ring = (off - (radius - rim) + 0.5).clamp(0., 1.) * disc;
            let white = ring.max(inside(arrow, px, py)) * 0.95;
            let pixel = poster.get_pixel_mut(x, y);
            *pixel = Rgb(pixel.0.map(|channel| {
                let dark = f32::from(channel) * (1. - 0.6 * disc);
                (dark + (255. - dark) * white).round() as u8
            }));
        }
    }
    poster
}

// How much of the pixel at (x, y) the triangle covers, its corners clockwise on screen.
fn inside(corners: [(f32, f32); 3], x: f32, y: f32) -> f32 {
    let edge = |(ax, ay): (f32, f32), (bx, by): (f32, f32)| {
        ((bx - ax) * (y - ay) - (by - ay) * (x - ax)) / (bx - ax).hypot(by - ay)
    };
    let [a, b, c] = corners;
    let nearest = edge(a, b).min(edge(b, c)).min(edge(c, a));
    (nearest + 0.5).clamp(0., 1.)
}

// Catmull-Rom, which the image crate widens by the scale, so every source pixel counts.
fn scale(image: &DynamicImage, width: u32) -> RgbImage {
    let height = (u64::from(image.height()) * u64::from(width) / u64::from(image.width())) as u32;
    image.resize_exact(width, height, FilterType::CatmullRom).into_rgb8()
}

fn create(path: &Path) -> Result<BufWriter<File>, String> {
    File::create(path).map(BufWriter::new).map_err(|e| format!("{}: {e}", path.display()))
}
