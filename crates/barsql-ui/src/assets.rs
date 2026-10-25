use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

// Lucide icons missing from GPUI Kit's default set.
icon_assets!(
    pub ExtraIcons,
    [
        Activity,
        ArrowDownAZ,
        Ban,
        Bookmark,
        BookmarkPlus,
        Box,
        Braces,
        Bug,
        CalendarPlus,
        ChartColumn,
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
        FolderPlus,
        Funnel,
        Gauge,
        GitBranch,
        Hash,
        KeyRound,
        Link,
        ListTree,
        Lock,
        MessageSquareText,
        Minimize2,
        NotebookText,
        Pencil,
        Pin,
        PinOff,
        Plug,
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

// The app icon, and its stack without the tile for the title bar, in tones that hold up on the dark and the light
// bar. Rendered from packaging/icon.svg.
pub const LOGO: &str = "images/barsql-icon.png";
pub const MARK_DARK: &str = "images/barsql-mark-dark.png";
pub const MARK_LIGHT: &str = "images/barsql-mark-light.png";
const IMAGES: [(&str, &[u8]); 3] = [
    (LOGO, include_bytes!("../images/barsql-icon.png")),
    (MARK_DARK, include_bytes!("../images/barsql-mark-dark.png")),
    (MARK_LIGHT, include_bytes!("../images/barsql-mark-light.png")),
];

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = IMAGES.iter().find(|(image, _)| *image == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
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
    use gpui_kit::AssetSource as _;
    use gpui_kit::assets::IconName as Lucide;

    use super::AppAssets;

    // An icon the app uses but doesn't ship draws as nothing at all.
    #[test]
    fn the_icons_the_status_bar_and_toolbars_use_are_shipped() {
        for icon in [Lucide::CircleQuestionMark, Lucide::Check, Lucide::Lock, Lucide::ShieldCheck, Lucide::LoaderCircle]
        {
            let path = icon.path();
            assert!(AppAssets.load(&path).ok().flatten().is_some(), "{path}");
        }
    }
}
