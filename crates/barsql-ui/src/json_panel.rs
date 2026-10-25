use std::time::Duration;

use barsql_io::{match_ranges, row_json};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    Editor, EditorState, InputEvent, InputState, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, TITLE_BAR_HEIGHT, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::{ToggleJsonPanel, shortcut_label};
use crate::form;
use crate::grid::RowRef;
use crate::i18n::{I18n, count, t, t_with};
use crate::scrollbars::ScrollbarsOnHover as _;
use crate::state::{set_setting_bool, setting_bool};
use crate::tokens::{ICON_SM, TINT_BORDER};
use crate::{theme, toast};

const FILTER_DEBOUNCE: Duration = Duration::from_millis(150);
const HIDE_NULLS_KEY: &str = "barsql-json-hide-nulls";

// Filters by key or value. Wrapping the filter in slashes makes it a regex.
pub struct JsonPanel {
    row: Option<RowRef>,
    filter: Entity<InputState>,
    editor: Entity<EditorState>,
    // Filter matches in `shown`.
    marks: RangeDecorationCollection,
    shown: String,
    // False while `shown` is the no-match note.
    matched: bool,
    hide_nulls: bool,
    debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl JsonPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t(cx, "jsonViewer.filterPlaceholder")));
        let editor =
            cx.new(|cx| EditorState::new(window, cx).language("json").line_number(false).soft_wrap(true).folding(true));
        let marks = editor.update(cx, |state, cx| state.create_range_decorations_collection(Vec::new(), cx));
        let subscriptions = vec![
            cx.subscribe_in(&filter, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.schedule_refresh(window, cx);
                }
            }),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                this.filter
                    .update(cx, |state, cx| state.set_placeholder(t(cx, "jsonViewer.filterPlaceholder"), window, cx));
                this.refresh(window, cx);
            }),
        ];
        Self {
            row: None,
            filter,
            editor,
            marks,
            shown: String::new(),
            matched: false,
            hide_nulls: setting_bool(cx, HIDE_NULLS_KEY, false),
            debounce: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn show(&mut self, row: Option<RowRef>, window: &mut Window, cx: &mut Context<Self>) {
        self.row = row;
        self.refresh(window, cx);
    }

    fn schedule_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(FILTER_DEBOUNCE).await;
            this.update_in(cx, |this, window, cx| this.refresh(window, cx)).ok();
        }));
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = &self.row else {
            cx.notify();
            return;
        };
        let query = self.filter.read(cx).value();
        let json = row_json(&row.set, row.row, &row.columns, &query, self.hide_nulls);
        self.matched = json.is_some();
        let text = json.unwrap_or_else(|| t(cx, "jsonViewer.noMatch").to_string());
        if text != self.shown {
            self.shown = text.clone();
            self.editor.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        // Set after the text, since replacing it would shift them.
        let color = cx.theme().primary.opacity(TINT_BORDER);
        let found = if self.matched { match_ranges(&self.shown, &query) } else { Vec::new() };
        let marks = found
            .into_iter()
            .map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(color))
            .collect();
        self.marks.set(marks, cx);
        cx.notify();
    }

    fn toggle_nulls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_nulls = !self.hide_nulls;
        set_setting_bool(cx, HIDE_NULLS_KEY, self.hide_nulls);
        self.refresh(window, cx);
    }

    // Copies what's shown, filter applied.
    fn copy(&self, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.shown.clone()));
        toast::success(t(cx, "toast.copiedClipboard"), cx);
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn set_filter(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |filter, cx| filter.set_value(text.to_string(), window, cx));
        self.refresh(window, cx);
    }

    #[cfg(test)]
    pub fn text(&self) -> Option<&str> {
        self.row.as_ref().map(|_| self.shown.as_str())
    }
}

impl Render for JsonPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let shortcut = shortcut_label(&ToggleJsonPanel, window).unwrap_or_default();
        let close = t_with(cx, "tooltip.closeJsonViewer", &[("shortcut", &shortcut)]);
        let number = self.row.as_ref().map(|row| t_with(cx, "jsonViewer.row", &[("number", &count(cx, row.number))]));
        // The header lines up with the tab bar, the filter row with the toolbar, and the buttons with the window's.
        let column = |button: Button| h_flex().flex_none().w(TITLE_BAR_HEIGHT).justify_center().child(button);
        let nulls_tip = t(cx, if self.hide_nulls { "jsonViewer.showNulls" } else { "jsonViewer.hideNulls" });
        let header = h_flex()
            .debug_selector(|| "json-panel-header".into())
            .h(rems(2.769))
            .flex_none()
            .pl(rems(0.769))
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(h_flex().min_w_0().gap(rems(0.615)).child(form::caption(t(cx, "jsonViewer.title"), cx)).children(
                number.map(|number| {
                    form::badge(number.to_uppercase(), theme.foreground).font_family(theme.mono_font_family.clone())
                }),
            ))
            .child(
                h_flex()
                    .flex_none()
                    .when(self.row.is_some(), |el| {
                        el.child(column(
                            Button::new("json-hide-nulls")
                                .ghost()
                                .small()
                                .icon(Icon::new(Lucide::EyeOff))
                                .selected(self.hide_nulls)
                                .tooltip(nulls_tip)
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_nulls(window, cx))),
                        ))
                        .child(column(
                            Button::new("json-panel-copy")
                                .ghost()
                                .small()
                                .icon(Icon::new(Lucide::Copy))
                                .tooltip(t(cx, "jsonViewer.copy"))
                                .disabled(!self.matched)
                                .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                        ))
                    })
                    .child(column(
                        Button::new("json-panel-close")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::Close))
                            .tooltip(close)
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleJsonPanel), cx)),
                    )),
            );
        let body = match self.row {
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_color(theme.muted_foreground)
                .child(t(cx, "jsonViewer.emptySelectRow"))
                .into_any_element(),
            Some(_) => v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    h_flex()
                        .h(rems(3.077))
                        .flex_none()
                        .gap(rems(0.615))
                        .px(rems(0.769))
                        .border_b_1()
                        .border_color(theme.border)
                        .debug_selector(|| "json-filter".into())
                        .child(Icon::new(IconName::Search).size(ICON_SM).text_color(theme.muted_foreground))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(form::filter_input(&self.filter, window, cx).cleanable(true)),
                        ),
                )
                .child(
                    div().flex_1().min_h_0().child(
                        Editor::new(&self.editor)
                            .readonly(true)
                            .context_menu(crate::context_menu::editor(&self.editor))
                            .size_full()
                            .bordered(false)
                            .text_size(px(theme::editor_font_size(cx)))
                            .scrollbars_on_hover(),
                    ),
                )
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .debug_selector(|| "json-panel".into())
            .bg(theme.sidebar)
            .border_l_1()
            .border_color(theme.border)
            .child(header)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use barsql_db::{ChunkBuilder, ColumnMeta, ResultSet};
    use gpui_kit::component::TITLE_BAR_HEIGHT;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext, px};

    use super::JsonPanel;
    use crate::grid::RowRef;
    use crate::test_support::Env;

    fn row() -> RowRef {
        row_with(Some("{\"nick\":\"ann\"}"))
    }

    // (id 7, name), with a NULL name for None.
    fn row_with(name: Option<&str>) -> RowRef {
        let columns: Arc<[ColumnMeta]> =
            ["id", "name"].map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into() }).to_vec().into();
        let mut builder = ChunkBuilder::new(2, 1);
        builder.push_number(|s| s.push('7'));
        match name {
            Some(name) => builder.push_text(|s| s.push_str(name)),
            None => builder.push_null(),
        }
        builder.end_row();
        let mut set = ResultSet::new(columns);
        set.push(Arc::new(builder.finish()));
        RowRef { set, row: 0, columns: vec![0, 1], number: 1 }
    }

    fn shown_text(panel: &Entity<JsonPanel>, cx: &mut VisualTestContext) -> Option<String> {
        panel.read_with(cx, |panel, _| panel.text().map(str::to_string))
    }

    fn set_filter(panel: &Entity<JsonPanel>, text: &'static str, cx: &mut VisualTestContext) {
        panel.update_in(cx, |panel, window, cx| {
            panel.filter.update(cx, |state, cx| state.set_value(text, window, cx));
            panel.refresh(window, cx);
        });
    }

    #[gpui_kit::test]
    fn copy_puts_the_json_shown_on_the_clipboard(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), window, cx));
        set_filter(&panel, "nick", cx);
        cx.update(|window, cx| window.click("json-panel-copy", cx));
        let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
        assert_eq!(copied.as_deref(), Some("{\n  \"name\": {\n    \"nick\": \"ann\"\n  }\n}"));
    }

    #[gpui_kit::test]
    fn the_null_switch_hides_null_columns_and_is_remembered(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row_with(None)), window, cx));
        assert_eq!(shown_text(&panel, cx).as_deref(), Some("{\n  \"id\": 7,\n  \"name\": null\n}"));
        cx.update(|window, cx| window.click("json-hide-nulls", cx));
        assert_eq!(shown_text(&panel, cx).as_deref(), Some("{\n  \"id\": 7\n}"));
        let next = cx.update(|window, cx| cx.new(|cx| JsonPanel::new(window, cx)));
        next.update_in(cx, |panel, window, cx| panel.show(Some(row_with(None)), window, cx));
        assert_eq!(shown_text(&next, cx).as_deref(), Some("{\n  \"id\": 7\n}"));
    }

    #[gpui_kit::test]
    fn the_filter_marks_what_it_matched(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), window, cx));
        set_filter(&panel, "NICK", cx);
        let marked = panel.read_with(cx, |panel, cx| {
            panel.marks.get_ranges(cx).into_iter().map(|range| panel.shown[range].to_string()).collect::<Vec<_>>()
        });
        assert_eq!(marked, ["nick"]);
        set_filter(&panel, "", cx);
        assert!(panel.read_with(cx, |panel, cx| panel.marks.get_ranges(cx)).is_empty(), "no filter, no marks");
    }

    #[gpui_kit::test]
    fn the_panel_shows_the_row_and_filters_it(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        let text = |cx: &mut VisualTestContext| panel.read_with(cx, |panel, _| panel.text().map(str::to_string));
        assert_eq!(text(cx), None);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), window, cx));
        assert_eq!(text(cx).as_deref(), Some("{\n  \"id\": 7,\n  \"name\": {\n    \"nick\": \"ann\"\n  }\n}"));
        panel.update_in(cx, |panel, window, cx| {
            panel.filter.update(cx, |state, cx| state.set_value("NICK", window, cx));
            panel.refresh(window, cx);
        });
        assert_eq!(text(cx).as_deref(), Some("{\n  \"name\": {\n    \"nick\": \"ann\"\n  }\n}"));
        panel.update_in(cx, |panel, window, cx| {
            panel.filter.update(cx, |state, cx| state.set_value("zzz", window, cx));
            panel.refresh(window, cx);
        });
        assert_eq!(text(cx).as_deref(), Some("// No key or value matches the filter"));
        panel.update_in(cx, |panel, window, cx| panel.show(None, window, cx));
        assert_eq!(text(cx), None);
    }

    // The window's buttons are TITLE_BAR_HEIGHT-wide columns at the right edge. Smallest, default and largest zoom.
    #[gpui_kit::test]
    fn the_header_buttons_sit_under_the_windows_buttons(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), window, cx));
        for zoom in [10., 13., 22.] {
            cx.update(|window, cx| {
                window.set_rem_size(px(zoom));
                window.render_frame(cx);
                let close_y = window.find("json-panel-close").bounds().center().y;
                for (column, id) in ["json-panel-close", "json-panel-copy", "json-hide-nulls"].into_iter().enumerate() {
                    let center = window.find(id).bounds().center();
                    let under = window.viewport_size().width - TITLE_BAR_HEIGHT * (column as f32 + 0.5);
                    assert!((center.x - under).abs() < px(0.5), "zoom {zoom}, {id}: {:?} vs {under:?}", center.x);
                    assert!((center.y - close_y).abs() < px(0.5), "zoom {zoom}, {id} is in the header");
                }
            });
        }
    }

    #[gpui_kit::test]
    fn the_filter_clears_with_its_x(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| {
            panel.show(Some(row()), window, cx);
            panel.filter.update(cx, |state, cx| state.set_value("nick", window, cx));
        });
        // GPUI Kit's clear button.
        cx.update(|window, cx| window.click("clean", cx));
        cx.executor().advance_clock(super::FILTER_DEBOUNCE);
        cx.run_until_parked();
        let (filter, text) = panel
            .read_with(cx, |panel, cx| (panel.filter.read(cx).value().to_string(), panel.text().map(str::to_string)));
        assert_eq!(filter, "");
        assert_eq!(text.as_deref(), Some("{\n  \"id\": 7,\n  \"name\": {\n    \"nick\": \"ann\"\n  }\n}"));
    }

    // The editor element sets read-only on its state as it draws, so the element has to ask for it.
    #[gpui_kit::test]
    fn the_json_cannot_be_typed_into(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), window, cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let editor = panel.read_with(cx, |panel, _| panel.editor.clone());
        editor.update_in(cx, |state, window, cx| state.focus(window, cx));
        let value = |cx: &mut VisualTestContext| editor.read_with(cx, |state, _| state.value().to_string());
        let before = value(cx);
        cx.simulate_input("x");
        assert_eq!(value(cx), before);
    }
}
