// Every icon file from packaging/icon.svg, each size rendered from the vector rather than scaled from another.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use resvg::usvg;
use tiny_skia::{Pixmap, Transform};

use crate::report;

const MASTER: &str = "packaging/icon.svg";
// The master at one size: the landing page.
const PNGS: [(&str, u32); 1] = [("docs/icon.png", 256)];
// The master as is, for the app to rasterize at each size it draws it, and for the landing page.
const COPIES: [&str; 2] = ["crates/barsql-ui/images/barsql-icon.svg", "docs/icon.svg"];
// Windows and Linux show the stack without its tile. Their taskbars and docks are mostly dark, where the dark tile
// disappears and the stack alone looked small beside other apps' icons.
const LINUX_PNGS: [(&str, u32); 3] = [
    ("packaging/linux/barsql-128.png", 128),
    ("packaging/linux/barsql-256.png", 256),
    ("packaging/linux/barsql-512.png", 512),
];
const ICO_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];
const ICNS_ENTRIES: [(&[u8; 4], u32); 10] = [
    (b"icp4", 16),
    (b"ic11", 32),
    (b"icp5", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];
// Apple's grid puts an 824px body on a 1024px canvas; the tile and its stroke span 488 of the master's 512.
const MACOS_SCALE: f64 = 824.0 / 1024.0 * 512.0 / 488.0;
const MACOS_SHADOW: &str = concat!(
    r#"<filter id="shadow" x="-10%" y="-10%" width="120%" height="125%">"#,
    r##"<feDropShadow dx="0" dy="5" stdDeviation="6" flood-color="#000" flood-opacity="0.3"/></filter>"##,
);

// The master's stack without its tile, framed square around its centre, for Windows, Linux and the title bar's marks.
const STACK_VIEW_BOX: &str = "66 66 380 380";

// The ellipse is the top face; the paths are the disks from the top down.
struct Tones {
    top: &'static str,
    disks: [&'static str; 3],
}

// The master's own blues, for dark themes, Windows and Linux.
const DARK: Tones = Tones { top: "#93c5fd", disks: ["#60a5fa", "#3b82f6", "#2563eb"] };
// A shade deeper, so the stack holds up on light ones.
const LIGHT: Tones = Tones { top: "#3b82f6", disks: ["#2563eb", "#1d4ed8", "#1e40af"] };
const MARKS: [(&str, Tones); 2] =
    [("crates/barsql-ui/images/barsql-mark-dark.svg", DARK), ("crates/barsql-ui/images/barsql-mark-light.svg", LIGHT)];

pub fn run(root: &Path) -> Result<(), String> {
    let source = fs::read_to_string(root.join(MASTER)).map_err(|e| format!("{MASTER}: {e}"))?;
    let master = parse(&source, MASTER)?;
    for (path, size) in PNGS {
        write(&root.join(path), &render(&master, size, MASTER)?)?;
    }
    for path in COPIES {
        write(&root.join(path), source.as_bytes())?;
    }

    let stack = parse(&stack_variant(&source, &DARK)?, "the stack")?;
    for (path, size) in LINUX_PNGS {
        write(&root.join(path), &render(&stack, size, "the stack")?)?;
    }
    let ico = ICO_SIZES.iter().map(|&size| Ok((size, render(&stack, size, "the stack")?)));
    write(&root.join("packaging/windows/icon.ico"), &ico_file(&ico.collect::<Result<Vec<_>, String>>()?))?;

    // Each size once, for every entry that shares it.
    let macos = parse(&macos_variant(&source)?, "the macOS icon")?;
    let sizes: BTreeSet<u32> = ICNS_ENTRIES.iter().map(|(_, size)| *size).collect();
    let pngs = sizes.into_iter().map(|size| Ok((size, render(&macos, size, "the macOS icon")?)));
    let pngs = pngs.collect::<Result<BTreeMap<_, _>, String>>()?;
    write(&root.join("packaging/macos/icons.icns"), &icns_file(ICNS_ENTRIES.map(|(kind, size)| (kind, &pngs[&size]))))?;

    for (path, tones) in MARKS {
        let mark = stack_variant(&source, &tones)?;
        // A broken variant fails here rather than as a blank mark in the app.
        parse(&mark, path)?;
        write(&root.join(path), mark.as_bytes())?;
    }
    Ok(())
}

// The master's opening tag, and the elements between it and </svg>.
fn split(source: &str) -> Result<(&str, &str), String> {
    let open = source.find("<svg").ok_or(format!("{MASTER}: no <svg>"))?;
    let start = open + source[open..].find('>').ok_or(format!("{MASTER}: unclosed <svg>"))? + 1;
    let end = source.rfind("</svg>").ok_or(format!("{MASTER}: no </svg>"))?;
    Ok((&source[..start], &source[start..end]))
}

// The master shrunk onto Apple's grid, over a drop shadow.
fn macos_variant(source: &str) -> Result<String, String> {
    let (open, content) = split(source)?;
    let transform = format!("translate(256 256) scale({MACOS_SCALE}) translate(-256 -256)");
    let body = format!("<g filter=\"url(#shadow)\"><g transform=\"{transform}\">{content}</g></g>");
    Ok(format!("{open}<defs>{MACOS_SHADOW}</defs>{body}</svg>"))
}

// The master's elements, one per line, less the tile, recoloured.
fn stack_variant(source: &str, tones: &Tones) -> Result<String, String> {
    let (_, content) = split(source)?;
    let mut disks = tones.disks.iter();
    let mut stack = String::new();
    for element in content.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with("<rect")) {
        let fill = if element.starts_with("<ellipse") {
            tones.top
        } else if element.starts_with("<path") {
            disks.next().ok_or(format!("{MASTER}: more than {} disks", tones.disks.len()))?
        } else {
            return Err(format!("{MASTER}: no stack colour for {element}"));
        };
        stack.push_str(&with_fill(element, fill)?);
    }
    if disks.next().is_some() {
        return Err(format!("{MASTER}: fewer than {} disks", tones.disks.len()));
    }
    Ok(format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{STACK_VIEW_BOX}\">{stack}</svg>"))
}

fn with_fill(element: &str, color: &str) -> Result<String, String> {
    let start = element.find("fill=\"").ok_or(format!("{MASTER}: no fill on {element}"))? + "fill=\"".len();
    let end = start + element[start..].find('"').ok_or(format!("{MASTER}: unclosed fill on {element}"))?;
    Ok(format!("{}{color}{}", &element[..start], &element[end..]))
}

fn parse(svg: &str, name: &str) -> Result<usvg::Tree, String> {
    usvg::Tree::from_str(svg, &usvg::Options::default()).map_err(|e| format!("{name}: {e}"))
}

// A square PNG of the whole view box, `size` pixels a side.
fn render(tree: &usvg::Tree, size: u32, name: &str) -> Result<Vec<u8>, String> {
    let mut pixmap = Pixmap::new(size, size).ok_or(format!("{name}: no {size}px canvas"))?;
    let scale = size as f32 / tree.size().width();
    resvg::render(tree, Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| format!("{name} at {size}px: {e}"))
}

// PNG entries behind a 32-bit directory, where a 256px side is written as 0.
fn ico_file(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let count = images.len() as u16;
    let mut file = [0u16, 1, count].iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<_>>();
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in images {
        let side = if *size >= 256 { 0 } else { *size as u8 };
        file.extend([side, side, 0, 0]);
        file.extend(1u16.to_le_bytes());
        file.extend(32u16.to_le_bytes());
        file.extend((png.len() as u32).to_le_bytes());
        file.extend(offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        file.extend(png);
    }
    file
}

// PNG entries in the given order, each behind its type and length.
fn icns_file(entries: [(&[u8; 4], &Vec<u8>); ICNS_ENTRIES.len()]) -> Vec<u8> {
    let length = 8 + entries.iter().map(|(_, png)| 8 + png.len() as u32).sum::<u32>();
    let mut file = b"icns".to_vec();
    file.extend(length.to_be_bytes());
    for (kind, png) in entries {
        file.extend(kind);
        file.extend((8 + png.len() as u32).to_be_bytes());
        file.extend(png);
    }
    file
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    report(path);
    Ok(())
}
