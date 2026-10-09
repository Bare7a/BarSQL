use std::rc::Rc;

use barsql_core::{ColumnInfo, ConnectionConfig};
use barsql_sql::schema_diff::{ColumnDiff, TableDiff, diff, sync_script};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::i18n::{t, t_count, t_with};
use crate::modal::{self, Modal};
use crate::schema::{self, Schemas};
use crate::state;
use crate::toast;
use crate::tokens::{ICON_XS, TEXT_SM, TEXT_XS};

// Tables a side loads.
const MAX_TABLES: usize = 500;

type OnScript = Rc<dyn Fn(ConnectionConfig, String, &mut Window, &mut App)>;

enum Compare {
    Idle,
    Loading,
    Ready { diffs: Vec<TableDiff>, script: String },
    Failed(String),
}

// Compares one schema with another of the same kind of database, and writes the SQL that brings the second up to
// the first.
pub struct SchemaDiffView {
    source: ConnectionConfig,
    source_schema: String,
    targets: Vec<ConnectionConfig>,
    target: usize,
    target_schema: Option<String>,
    state: Compare,
    scroll: ScrollHandle,
    on_script: OnScript,
    _schemas: Subscription,
    _task: Task<()>,
}

impl SchemaDiffView {
    fn new(source: ConnectionConfig, source_schema: String, on_script: OnScript, cx: &mut Context<Self>) -> Self {
        // The source's own connection first, so another schema of it is one pick away.
        let mut targets: Vec<ConnectionConfig> = state::bar(cx)
            .list_connections()
            .into_iter()
            .filter(|c| c.driver == source.driver && c.id != source.id)
            .collect();
        targets.insert(0, source.clone());
        let mut this = Self {
            source,
            source_schema,
            targets,
            target: 0,
            target_schema: None,
            state: Compare::Idle,
            scroll: ScrollHandle::new(),
            on_script,
            _schemas: cx.observe_global::<Schemas>(|this, cx| {
                this.pick_default_schema(cx);
                cx.notify();
            }),
            _task: Task::ready(()),
        };
        this.set_target(0, cx);
        this
    }

    fn target(&self) -> &ConnectionConfig {
        &self.targets[self.target]
    }

    fn schemas(&self, cx: &App) -> Option<Vec<String>> {
        let target = self.target();
        let schemas = schema::get(cx, &target.id)?.schemas()?;
        Some(schemas.iter().map(|s| s.name.clone()).collect())
    }

    fn set_target(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.target = ix.min(self.targets.len() - 1);
        self.target_schema = None;
        self.state = Compare::Idle;
        let target = self.target().clone();
        schema::ensure_loaded(&target.id, target.driver.clone(), cx);
        self.pick_default_schema(cx);
        cx.notify();
    }

    // Another schema of the same connection, or the same-named one elsewhere.
    fn pick_default_schema(&mut self, cx: &App) {
        if self.target_schema.is_some() {
            return;
        }
        let Some(schemas) = self.schemas(cx) else { return };
        let same = self.target().id == self.source.id;
        self.target_schema = match same {
            true => schemas.iter().find(|s| **s != self.source_schema).cloned(),
            false => schemas.iter().find(|s| **s == self.source_schema).or(schemas.first()).cloned(),
        };
    }

    fn compare(&mut self, cx: &mut Context<Self>) {
        let Some(target_schema) = self.target_schema.clone() else { return };
        let target = self.target().clone();
        let source = schema::load_schema_columns(&self.source.id, &self.source_schema, MAX_TABLES, cx);
        let other = schema::load_schema_columns(&target.id, &target_schema, MAX_TABLES, cx);
        self.state = Compare::Loading;
        self._task = cx.spawn(async move |this, cx| {
            let (source, other) = (source.await, other.await);
            let _ = this.update(cx, |this, cx| {
                this.state = match (source, other) {
                    (Ok((_, source)), Ok((_, other))) => {
                        let diffs = diff(&source, &other);
                        let script = sync_script(&target.driver, &target_schema, &diffs);
                        Compare::Ready { diffs, script }
                    }
                    (Err(error), _) | (_, Err(error)) => Compare::Failed(error),
                };
                cx.notify();
            });
        });
        cx.notify();
    }

    fn pickers(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity().downgrade();
        let connections: Vec<(usize, SharedString)> =
            self.targets.iter().enumerate().map(|(ix, c)| (ix, SharedString::from(c.name.clone()))).collect();
        let current = self.target;
        let connection = Button::new("diff-target-connection")
            .debug_selector(|| "diff-target-connection".into())
            .small()
            .outline()
            .label(self.target().name.clone())
            .dropdown_caret(true)
            .dropdown_menu({
                let this = this.clone();
                move |mut menu: PopupMenu, _, _| {
                    for (ix, name) in &connections {
                        let (this, ix) = (this.clone(), *ix);
                        menu = menu.item(PopupMenuItem::new(name.clone()).checked(current == ix).on_click(
                            move |_, _, cx| {
                                let _ = this.update(cx, |view, cx| view.set_target(ix, cx));
                            },
                        ));
                    }
                    menu
                }
            });
        let schemas = self.schemas(cx).unwrap_or_default();
        let picked = self.target_schema.clone();
        let schema = Button::new("diff-target-schema")
            .debug_selector(|| "diff-target-schema".into())
            .small()
            .outline()
            .label(picked.clone().unwrap_or_else(|| t(cx, "schemaDiff.pickSchema").to_string()))
            .dropdown_caret(true)
            .disabled(schemas.is_empty())
            .dropdown_menu(move |mut menu: PopupMenu, _, _| {
                for name in &schemas {
                    let (this, name) = (this.clone(), name.clone());
                    let checked = picked.as_deref() == Some(name.as_str());
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(checked).on_click(move |_, _, cx| {
                        let _ = this.update(cx, |view, cx| {
                            view.target_schema = Some(name.clone());
                            view.state = Compare::Idle;
                            cx.notify();
                        });
                    }));
                }
                menu
            });
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .flex_wrap()
            .gap(rems(0.615))
            .px(rems(1.231))
            .py(rems(0.769))
            .border_b_1()
            .border_color(theme.border)
            .text_size(TEXT_SM)
            .child(div().font_semibold().child(format!("{} · {}", self.source.name, self.source_schema)))
            .child(div().text_color(theme.muted_foreground).child(t(cx, "schemaDiff.with")))
            .child(connection)
            .child(schema)
            .child(
                Button::new("diff-compare")
                    .debug_selector(|| "diff-compare".into())
                    .small()
                    .primary()
                    .label(t(cx, "schemaDiff.compare"))
                    .disabled(self.target_schema.is_none() || matches!(self.state, Compare::Loading))
                    .on_click(cx.listener(|view, _, _, cx| view.compare(cx))),
            )
    }

    fn results(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let note =
            |text: SharedString| div().p(rems(1.231)).text_color(theme.muted_foreground).child(text).into_any_element();
        let Compare::Ready { diffs, .. } = &self.state else {
            return match &self.state {
                Compare::Loading => note(t(cx, "schemaDiff.loading")),
                Compare::Failed(error) => note(error.clone().into()),
                _ => note(t(cx, "schemaDiff.hint")),
            };
        };
        if diffs.is_empty() {
            return note(t(cx, "schemaDiff.same"));
        }
        let target = self.target_schema.clone().unwrap_or_default();
        let (added, removed, changed) = (theme.success, theme.danger, theme.warning);
        let line = |icon: Lucide, color: Hsla, text: String, detail: String, indent: bool| {
            h_flex()
                .gap(rems(0.462))
                .py(rems(0.231))
                .when(indent, |el| el.pl(rems(1.538)))
                .child(Icon::new(icon).size(ICON_XS).text_color(color))
                .child(div().min_w_0().truncate().child(text))
                .child(div().flex_none().text_size(TEXT_XS).text_color(theme.muted_foreground).child(detail))
        };
        let type_of = |c: &ColumnInfo| c.data_type.to_lowercase();
        let mut rows: Vec<Div> = Vec::new();
        for table in diffs {
            match table {
                TableDiff::Missing { name, columns } => rows.push(line(
                    Lucide::Plus,
                    added,
                    name.clone(),
                    t_with(
                        cx,
                        "schemaDiff.missingTable",
                        &[("schema", &target), ("count", &columns.len().to_string())],
                    )
                    .to_string(),
                    false,
                )),
                TableDiff::Extra { name } => rows.push(line(
                    Lucide::Minus,
                    removed,
                    name.clone(),
                    t_with(cx, "schemaDiff.extraTable", &[("schema", &target)]).to_string(),
                    false,
                )),
                TableDiff::Changed { name, columns } => {
                    let count = t_count(cx, "schemaDiff.changedTable", columns.len() as i64, &[]).to_string();
                    rows.push(line(Lucide::Pencil, changed, name.clone(), count, false));
                    for column in columns {
                        rows.push(match column {
                            ColumnDiff::Missing(c) => line(Lucide::Plus, added, c.name.clone(), type_of(c), true),
                            ColumnDiff::Extra(c) => line(Lucide::Minus, removed, c.name.clone(), type_of(c), true),
                            ColumnDiff::Changed { source, target } => line(
                                Lucide::Pencil,
                                changed,
                                source.name.clone(),
                                format!("{} → {}", describe(target), describe(source)),
                                true,
                            ),
                        });
                    }
                }
            }
        }
        let rows = rows.into_iter().enumerate().map(|(ix, row)| row.debug_selector(move || format!("diff-row-{ix}")));
        v_flex().px(rems(1.231)).py(rems(0.769)).text_size(TEXT_SM).children(rows).into_any_element()
    }
}

fn describe(column: &ColumnInfo) -> String {
    let mut text = column.data_type.to_lowercase();
    if !column.is_nullable {
        text.push_str(" not null");
    }
    if column.is_primary {
        text.push_str(" pk");
    }
    text
}

impl Render for SchemaDiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let script = match &self.state {
            Compare::Ready { script, .. } if !script.is_empty() => Some(script.clone()),
            _ => None,
        };
        let body = v_flex().child(self.results(cx));
        let (copy, open) = (script.clone(), script.clone());
        let on_script = self.on_script.clone();
        let target = self.target().clone();
        modal::scroll_content().child(self.pickers(cx)).child(modal::scroll_body(&self.scroll, body)).child(
            modal::footer(cx)
                .child(
                    Button::new("diff-copy-script")
                        .large()
                        .icon(Icon::new(IconName::Copy))
                        .label(t(cx, "schemaDiff.copyScript"))
                        .disabled(copy.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(script) = &copy {
                                cx.write_to_clipboard(ClipboardItem::new_string(script.clone()));
                                toast::success(t(cx, "toast.copiedClipboard"), cx);
                            }
                        }),
                )
                .child(
                    Button::new("diff-open-script")
                        .large()
                        .primary()
                        .debug_selector(|| "diff-open-script".into())
                        .label(t(cx, "schemaDiff.openScript"))
                        .disabled(open.is_none())
                        .on_click(move |_, window, cx| {
                            if let Some(script) = &open {
                                window.close_dialog(cx);
                                on_script(target.clone(), script.clone(), window, cx);
                            }
                        }),
                ),
        )
    }
}

// `on_script` gets the target connection and the script, to open it in a tab without running it.
pub fn open(
    source: ConnectionConfig,
    schema: String,
    window: &mut Window,
    cx: &mut App,
    on_script: impl Fn(ConnectionConfig, String, &mut Window, &mut App) + 'static,
) {
    let title = t(cx, "schemaDiff.title");
    let view = cx.new(|cx| SchemaDiffView::new(source, schema, Rc::new(on_script), cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let height = window.viewport_size().height * 0.75;
        Modal::new("schema-diff", title.clone()).size(modal::Size::Xl).height(height).build(
            dialog,
            view.clone(),
            window,
            cx,
        )
    });
}
