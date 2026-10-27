use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::{Align, Placement, Positioner};
use gpui_kit::component::{ActiveTheme, Icon, ThemeStyled, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{Choices, Grid};

// Rows the list shows before it scrolls.
const VISIBLE_ROWS: f32 = 8.5;

// An enum's values under its cell's editor. The editor's text narrows them, the arrows move along them, and Enter
// or a click takes one.
pub(super) struct Picker {
    choices: Choices,
    // What the text keeps: indices into the values, and None for NULL.
    shown: Vec<Option<usize>>,
    highlighted: usize,
    // The arrows moved the highlight, so an open list's Enter takes the value, not the text.
    stepped: bool,
    scroll: ScrollHandle,
}

impl Picker {
    // Every value shows until something is typed, starting at the one the cell holds.
    pub(super) fn new(choices: Choices, held: Option<&str>) -> Self {
        let mut shown: Vec<Option<usize>> = (0..choices.values.len()).map(Some).collect();
        if choices.nullable {
            shown.push(None);
        }
        let highlighted =
            shown.iter().position(|&choice| choice.map(|ix| choices.values[ix].as_str()) == held).unwrap_or_default();
        Self { choices, shown, highlighted, stepped: false, scroll: ScrollHandle::new() }
    }

    // The values holding the text, ignoring case, and NULL while the text could still be the start of it.
    fn narrow(&mut self, text: &str) {
        let text = text.to_lowercase();
        let values = &self.choices.values;
        self.shown = (0..values.len()).filter(|&ix| values[ix].to_lowercase().contains(&text)).map(Some).collect();
        if self.choices.nullable && "null".starts_with(&text) {
            self.shown.push(None);
        }
        self.highlighted = 0;
        self.stepped = false;
        self.scroll.scroll_to_item(0);
    }

    pub(super) fn takes_text(&self) -> bool {
        self.choices.open && !self.stepped
    }

    // The value at `ix` in the list, Some(None) for NULL.
    pub(super) fn value(&self, ix: usize) -> Option<Option<String>> {
        self.shown.get(ix).map(|choice| choice.map(|ix| self.choices.values[ix].clone()))
    }

    pub(super) fn highlighted(&self) -> Option<Option<String>> {
        self.value(self.highlighted)
    }

    #[cfg(test)]
    pub(super) fn listed(&self) -> (Vec<Option<String>>, usize) {
        ((0..self.shown.len()).filter_map(|ix| self.value(ix)).collect(), self.highlighted)
    }

    // The value the text names, in any case. Half a value names none.
    pub(super) fn named(&self, text: &str) -> Option<String> {
        let values = &self.choices.values;
        let lower = text.to_lowercase();
        values
            .iter()
            .find(|value| *value == text)
            .or_else(|| values.iter().find(|value| value.to_lowercase() == lower))
            .cloned()
    }
}

impl Grid {
    pub(super) fn narrow_choices(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = &mut self.editing else { return };
        let Some(picker) = &mut editing.picker else { return };
        picker.narrow(&editing.input.read(cx).value());
        cx.notify();
    }

    // The arrows stop at either end.
    pub(super) fn step_choice(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(picker) = self.editing.as_mut().and_then(|editing| editing.picker.as_mut()) else { return };
        let Some(last) = picker.shown.len().checked_sub(1) else { return };
        picker.highlighted = picker.highlighted.saturating_add_signed(by).min(last);
        picker.stepped = true;
        picker.scroll.scroll_to_item(picker.highlighted);
        cx.notify();
    }

    fn highlight_choice(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(picker) = self.editing.as_mut().and_then(|editing| editing.picker.as_mut()) else { return };
        if picker.highlighted != ix {
            picker.highlighted = ix;
            cx.notify();
        }
    }

    // Under the cell, or over it where the window has no room below, while the cell is in sight. `cell` is in the
    // window.
    pub(super) fn picker_list(&self, cell: Bounds<Pixels>, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let editing = self.editing.as_ref()?;
        let picker = editing.picker.as_ref().filter(|_| self.scroll.viewport().intersects(&cell))?;
        let (m, theme) = (&self.metrics, cx.theme());
        let held = self.shown(editing.row, editing.column).display().map(str::to_string);
        let rows = picker.shown.iter().enumerate().map(|(ix, &choice)| {
            let highlighted = ix == picker.highlighted;
            let label = choice.map_or_else(|| SharedString::from("NULL"), |v| picker.choices.values[v].clone().into());
            let is_held = choice.map(|v| picker.choices.values[v].as_str()) == held.as_deref();
            h_flex()
                .id(("grid-choice", ix))
                .debug_selector(move || format!("grid-choice-{ix}"))
                .h(m.row_height)
                .px(m.cell_pad_x)
                .gap(m.gap)
                .rounded(m.radius)
                .cursor_pointer()
                .when(choice.is_none(), |row| row.text_color(theme.muted_foreground))
                .when(highlighted, |row| row.bg(theme.accent).text_color(theme.accent_foreground))
                .on_hover(cx.listener(move |grid, hovered: &bool, _, cx| {
                    if *hovered {
                        grid.highlight_choice(ix, cx);
                    }
                }))
                // Before the grid's own press, which would end the edit.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |grid, _, window, cx| {
                        cx.stop_propagation();
                        grid.pick_choice(ix, window, cx);
                    }),
                )
                .child(div().flex_1().min_w_0().truncate().child(label))
                .when(is_held, |row| row.child(Icon::new(Lucide::Check).size(m.icon)))
        });
        let list = v_flex()
            .id("grid-choices")
            .debug_selector(|| "grid-choices".into())
            .occlude()
            .min_w(cell.size.width)
            .max_w(rems(24.615))
            .max_h(m.row_height * VISIBLE_ROWS)
            .overflow_y_scroll()
            .track_scroll(&picker.scroll)
            .p(m.jump_inset)
            .popover_style(cx)
            .font_family(theme.mono_font_family.clone())
            .text_size(m.font_size)
            .children(rows);
        let place = Positioner::side(cell).placement(Placement::Bottom).align(Align::Start).offset(m.jump_inset);
        Some(deferred(place.margin(px(8.)).child(list)).with_priority(1))
    }
}
