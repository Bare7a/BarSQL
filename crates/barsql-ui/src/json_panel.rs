use std::time::Duration;

use barsql_io::row_json;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::*;

use crate::actions::{ToggleJsonPanel, shortcut_label};
use crate::form;
use crate::grid::RowRef;
use crate::i18n::{I18n, t, t_with};
use crate::scrollbars::ScrollbarsOnHover as _;
use crate::theme;
use crate::tokens::ICON_SM;

const FILTER_DEBOUNCE: Duration = Duration::from_millis(150);

// Filters by key or value. Wrapping the filter in slashes makes it a regex.
pub struct JsonPanel {
    row: Option<RowRef>,
    filter: Entity<InputState>,
    editor: Entity<EditorState>,
    shown: String,
    debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl JsonPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t(cx, "jsonViewer.filterPlaceholder")));
        let editor =
            cx.new(|cx| EditorState::new(window, cx).language("json").line_number(false).soft_wrap(true).folding(true));
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
        Self { row: None, filter, editor, shown: String::new(), debounce: None, _subscriptions: subscriptions }
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
        let text = row_json(&row.set, row.row, &row.columns, &query)
            .unwrap_or_else(|| t(cx, "jsonViewer.noMatch").to_string());
        if text != self.shown {
            self.shown = text.clone();
            self.editor.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        cx.notify();
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
        // Its lines continue the editor's: the header ends with the tab bar and the filter row with the toolbar.
        let header = h_flex()
            .debug_selector(|| "json-panel-header".into())
            .h(rems(2.769))
            .flex_none()
            .px(rems(0.769))
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(form::caption(t(cx, "jsonViewer.title"), cx))
            .child(
                Button::new("json-panel-close")
                    .ghost()
                    .small()
                    .icon(Icon::new(IconName::Close))
                    .tooltip(close)
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleJsonPanel), cx)),
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
                        .child(div().flex_1().min_w_0().child(form::filter_input(&self.filter, window, cx))),
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
    use gpui_kit::{TestAppContext, VisualTestContext};

    use super::JsonPanel;
    use crate::grid::RowRef;
    use crate::test_support::Env;

    fn row() -> RowRef {
        let columns: Arc<[ColumnMeta]> =
            ["id", "name"].map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into() }).to_vec().into();
        let mut builder = ChunkBuilder::new(2, 1);
        builder.push_number(|s| s.push('7'));
        builder.push_text(|s| s.push_str("{\"nick\":\"ann\"}"));
        builder.end_row();
        let mut set = ResultSet::new(columns);
        set.push(Arc::new(builder.finish()));
        RowRef { set, row: 0, columns: vec![0, 1] }
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
        assert_eq!(text(cx).as_deref(), Some("// No keys match filter"));
        panel.update_in(cx, |panel, window, cx| panel.show(None, window, cx));
        assert_eq!(text(cx), None);
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
