use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

// Lucide icons missing from GPUI Kit's default set.
icon_assets!(
    pub ExtraIcons,
    [
        ArrowDownAZ,
        Ban,
        Bookmark,
        BookmarkPlus,
        Box,
        Braces,
        CalendarPlus,
        CircleAlert,
        CirclePlay,
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
        ListTree,
        Lock,
        Minimize2,
        Pencil,
        Pin,
        PinOff,
        Plug,
        RefreshCw,
        Route,
        Save,
        Sheet,
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

pub const LOGO: &str = "images/barsql-icon.png";
const LOGO_BYTES: &[u8] = include_bytes!("../images/barsql-icon.png");

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            return Ok(Some(Cow::Borrowed(LOGO_BYTES)));
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
