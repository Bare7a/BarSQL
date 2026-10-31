use std::borrow::Cow;
use std::sync::Arc;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{
    App, AssetSource, Bounds, Canvas, Corners, DevicePixels, Global, Length, Pixels, RenderImage, Result, SharedString,
    Size, Styled, SvgSize, canvas, size,
};

// Lucide icons missing from GPUI Kit's default set.
icon_assets!(
    pub ExtraIcons,
    [
        Activity,
        ArrowDownAZ,
        Ban,
        Binary,
        Bookmark,
        BookmarkPlus,
        Box,
        Braces,
        Brackets,
        Bug,
        CalendarClock,
        CalendarPlus,
        ChartColumn,
        ChevronsDownUp,
        CircleAlert,
        CirclePlay,
        CircleQuestionMark,
        ClipboardCopy,
        CodeXml,
        Clock,
        Columns3,
        Crosshair,
        Database,
        DatabaseZap,
        Download,
        FileCode,
        FingerprintPattern,
        FolderPlus,
        Funnel,
        Gauge,
        GitBranch,
        Hash,
        KeyRound,
        Layers,
        Link,
        List,
        ListTree,
        Lock,
        MessageSquareText,
        Minimize2,
        NotebookText,
        Pencil,
        Pin,
        PinOff,
        Plug,
        Puzzle,
        RefreshCw,
        Route,
        Save,
        Scan,
        Sheet,
        ShieldCheck,
        Sigma,
        SkipForward,
        SlidersHorizontal,
        Square,
        SquareFunction,
        SquarePen,
        Table,
        Table2,
        TextAlignStart,
        ToggleLeft,
        Trash,
        Unplug,
        Upload,
        View,
        Workflow,
        X,
        Zap,
    ]
);

// Inter and Fira Code at 400/500/600, SIL OFL 1.1.
const FONTS: [&[u8]; 6] = [
    include_bytes!("../fonts/Inter-Regular.ttf"),
    include_bytes!("../fonts/Inter-Medium.ttf"),
    include_bytes!("../fonts/Inter-SemiBold.ttf"),
    include_bytes!("../fonts/FiraCode-Regular.ttf"),
    include_bytes!("../fonts/FiraCode-Medium.ttf"),
    include_bytes!("../fonts/FiraCode-SemiBold.ttf"),
];

pub fn load_fonts(cx: &mut gpui_kit::App) {
    let _ = cx.text_system().add_fonts(FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect());
}

// The app icon, and the title bar's tile-less marks for dark and light: packaging/icon.svg and its variants.
pub const LOGO: &[u8] = include_bytes!("../images/barsql-icon.svg");
pub const MARK_DARK: &[u8] = include_bytes!("../images/barsql-mark-dark.svg");
pub const MARK_LIGHT: &[u8] = include_bytes!("../images/barsql-mark-light.svg");

// Rasters by image and device size, made once each like GPUI's own svg().
#[derive(Default)]
struct Rasters(Vec<(&'static [u8], Size<DevicePixels>, Arc<RenderImage>)>);

impl Global for Rasters {}

// An SVG rasterized at the device pixels it covers. GPUI samples an image once per pixel with no mipmaps, so a bitmap
// shrunk past 2:1 draws jagged with a broken outline, and one stretched past its size draws soft.
pub fn vector_img(svg: &'static [u8], side: impl Clone + Into<Length>) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            if let Some(image) = raster(svg, device_size(bounds, window.scale_factor()), cx) {
                let _ = window.paint_image(bounds, bounds, Corners::default(), image, 0, false);
            }
        },
    )
    .size(side)
}

// The size paint_image snaps these bounds to, each edge rounded half toward zero, so the raster maps 1:1.
fn device_size(bounds: Bounds<Pixels>, scale: f32) -> Size<DevicePixels> {
    let edge = |logical: Pixels| {
        let device = f32::from(logical) * scale;
        (device.abs() - 0.5).ceil().copysign(device) as i32
    };
    let span = |near, far| DevicePixels((edge(far) - edge(near)).max(0));
    size(span(bounds.left(), bounds.right()), span(bounds.top(), bounds.bottom()))
}

fn raster(svg: &'static [u8], pixels: Size<DevicePixels>, cx: &mut App) -> Option<Arc<RenderImage>> {
    if pixels.width.0 == 0 || pixels.height.0 == 0 {
        return None;
    }
    let made =
        cx.try_global::<Rasters>().and_then(|rasters| rasters.0.iter().find(|(of, at, _)| *at == pixels && *of == svg));
    if let Some((_, _, image)) = made {
        return Some(image.clone());
    }
    let renderer = cx.svg_renderer();
    let image =
        renderer.parse_svg(svg).and_then(|tree| renderer.render_parsed(&tree, SvgSize::ExactSize(pixels))).ok()?;
    cx.default_global::<Rasters>().0.push((svg, pixels, image.clone()));
    Some(image)
}

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = Assets.list(path)?;
        all.extend(ExtraIcons.list(path)?);
        all.sort();
        all.dedup();
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::AssetSource as _;
    use gpui_kit::assets::IconName as Lucide;
    use gpui_kit::{Bounds, DevicePixels, TestAppContext, point, px, size};

    use super::{AppAssets, LOGO, MARK_DARK, MARK_LIGHT, device_size, raster};

    // A raster a pixel off the snapped bounds gets resampled. Each edge snaps on its own, ties toward zero.
    #[test]
    fn a_raster_covers_the_snapped_bounds() {
        let at = |origin: f32, side: f32, scale: f32| {
            let side = device_size(Bounds::new(point(px(origin), px(origin)), size(px(side), px(side))), scale);
            (side.width.0, side.height.0)
        };
        assert_eq!(at(10.3, 56., 1.25), (70, 70));
        // 4.308rem at 13px is 56.004px.
        assert_eq!(at(0., 56.004, 2.), (112, 112));
        assert_eq!(at(0.248, 56.004, 2.), (113, 113));
        assert_eq!(at(10.25, 56.1, 2.), (113, 113));
    }

    #[gpui_kit::test]
    fn each_image_is_rasterized_once_per_size(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let side = size(DevicePixels(20), DevicePixels(20));
            let images = [LOGO, MARK_DARK, MARK_LIGHT].map(|svg| raster(svg, side, cx).unwrap());
            for (svg, image) in [LOGO, MARK_DARK, MARK_LIGHT].into_iter().zip(&images) {
                assert_eq!(image.size(0), side);
                let alpha: Vec<u8> = image.as_bytes(0).unwrap().chunks(4).map(|pixel| pixel[3]).collect();
                assert_eq!(alpha[0], 0, "a clear corner");
                assert!(alpha.contains(&255), "something drawn");
                assert!(Arc::ptr_eq(image, &raster(svg, side, cx).unwrap()));
            }
            assert!(!Arc::ptr_eq(&images[0], &images[1]) && !Arc::ptr_eq(&images[1], &images[2]));

            let wider = size(DevicePixels(113), DevicePixels(112));
            assert_eq!(raster(LOGO, wider, cx).unwrap().size(0), wider);
            assert!(raster(LOGO, size(DevicePixels(0), DevicePixels(0)), cx).is_none());
        });
    }

    // An icon the app uses but doesn't ship draws as nothing at all.
    #[test]
    fn the_icons_the_status_bar_and_toolbars_use_are_shipped() {
        for icon in [
            Lucide::CircleQuestionMark,
            Lucide::Check,
            Lucide::Lock,
            Lucide::ShieldCheck,
            Lucide::LoaderCircle,
            Lucide::ChevronsDownUp,
            Lucide::Copy,
            Lucide::EyeOff,
        ] {
            let path = icon.path();
            assert!(AppAssets.load(&path).ok().flatten().is_some(), "{path}");
        }
    }
}
