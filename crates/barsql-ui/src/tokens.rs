use gpui_kit::{Rems, rems};

// Rem of the 13px root. GPUI's presets run a pixel or two smaller (`text_xs` is 9.75px, a small icon 11.4px),
// so views use these instead.
pub const TEXT_2XS: Rems = rems(0.769);
pub const TEXT_XS: Rems = rems(0.846);
pub const TEXT_SM: Rems = rems(0.923);
pub const TEXT_BASE: Rems = rems(1.);
pub const TEXT_MD: Rems = rems(1.077);
pub const TEXT_LG: Rems = rems(1.231);

pub const ICON_2XS: Rems = rems(0.846);
pub const ICON_XS: Rems = rems(0.923);
pub const ICON_SM: Rems = rems(1.077);
pub const ICON_MD: Rems = rems(1.231);
pub const ICON_XL: Rems = rems(2.308);

// SM for badges and keys, RADIUS for controls and small floating panels like menus, LG for dialogs.
pub const RADIUS_SM: Rems = rems(0.308);
pub const RADIUS: Rems = rems(0.462);
pub const RADIUS_LG: Rems = rems(0.615);

// The bars across the top of the sidebar, the editor and the JSON panel line up. A toolbar, header or filter bar
// under them is a STRIP.
pub const TOP_BAR: Rems = rems(2.769);
pub const STRIP: Rems = rems(3.077);

// A connection's state beside its name, and smaller marks like an unsaved tab's.
pub const DOT: Rems = rems(0.538);
pub const DOT_SM: Rems = rems(0.462);

// Alpha of a background or border tinted with the text's colour.
// DIMMED is the opacity of disabled or skipped elements.
pub const TINT: f32 = 0.12;
pub const TINT_BORDER: f32 = 0.4;
pub const DIMMED: f32 = 0.5;
