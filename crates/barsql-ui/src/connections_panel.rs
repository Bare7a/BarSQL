use std::collections::BTreeMap;

use barsql_app::ConnectionFolder;
use barsql_core::{ConnectionConfig, DriverType};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::dialogs::{self, Confirm, Prompt};
use crate::form;
use crate::i18n::{t, t_with};
use crate::list_nav::{self, ListNav, NavDelete, NavDown, NavFirst, NavLast, NavOpen, NavUp};
use crate::saved_queries::failed;
use crate::schema;
use crate::scrollbars::ScrollbarsOnHover as _;
use crate::state::{self, set_setting_json, setting_json};
use crate::tokens::{ICON_SM, ICON_XS, RADIUS, TEXT_BASE, TEXT_SM, TEXT_XS};

const COLLAPSED_KEY: &str = "barsql-folders-collapsed";

type PanelAction = Box<dyn Fn(&mut ConnectionsPanel, &mut Window, &mut Context<ConnectionsPanel>)>;

#[derive(Clone)]
struct ConnectionDrag {
    id: String,
    name: SharedString,
}

impl Render for ConnectionDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded(RADIUS)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_size(TEXT_SM)
            .opacity(0.9)
            .child(self.name.clone())
    }
}

fn drop_ring(style: StyleRefinement, cx: &App) -> StyleRefinement {
    style.shadow(vec![BoxShadow {
        color: cx.theme().primary,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(1.),
        inset: true,
    }])
}

pub enum ConnectionsEvent {
    // Goes to the connection's tab when connected, otherwise only selects it.
    Activate(String),
    Connected(String),
    // Opens the connection dialog. An empty id means a new or duplicated connection.
    Edit(Box<ConnectionConfig>),
    // Connections or folders changed on disk.
    Changed,
}

pub fn connection_subtitle(c: &ConnectionConfig) -> String {
    if c.driver == DriverType::Sqlite {
        // Split on both separators since the path may have been saved on another OS.
        let file = c.file_path.rsplit(['/', '\\']).next().unwrap_or_default();
        return if file.is_empty() { "sqlite".into() } else { format!("sqlite · {file}") };
    }
    let place =
        [c.host.as_str(), c.database.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/");
    if place.is_empty() { c.driver.to_string() } else { format!("{} · {place}", c.driver) }
}

pub fn parse_color(color: &str) -> Option<Hsla> {
    let hex = color.strip_prefix('#')?;
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten().map(|value| rgb(value).into())
}

// Loose connections first, then folders.
pub struct ConnectionsPanel {
    connections: Vec<ConnectionConfig>,
    folders: Vec<ConnectionFolder>,
    collapsed: BTreeMap<String, bool>,
    selected: Option<String>,
    nav: ListNav,
}

impl EventEmitter<ConnectionsEvent> for ConnectionsPanel {}

impl ConnectionsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            connections: Vec::new(),
            folders: Vec::new(),
            collapsed: setting_json(cx, COLLAPSED_KEY),
            selected: None,
            nav: ListNav::new(cx),
        }
    }

    // Connection ids in display order, each with the index of the list child it sits in.
    fn nav_rows(&self) -> Vec<(String, usize)> {
        let folder_ids: Vec<&str> = self.folders.iter().map(|f| f.id.as_str()).collect();
        let loose =
            self.connections.iter().filter(|c| c.folder_id.is_empty() || !folder_ids.contains(&c.folder_id.as_str()));
        let mut rows: Vec<(String, usize)> = loose.enumerate().map(|(ix, c)| (c.id.clone(), ix)).collect();
        let base = rows.len();
        for (offset, folder) in self.folders.iter().enumerate() {
            if self.collapsed.get(&folder.id) == Some(&true) {
                continue;
            }
            rows.extend(
                self.connections.iter().filter(|c| c.folder_id == folder.id).map(|c| (c.id.clone(), base + offset)),
            );
        }
        rows
    }

    pub fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav.focus_list(window, cx);
    }

    pub fn list_focus(&self) -> &FocusHandle {
        &self.nav.focus
    }

    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let rows = self.nav_rows();
        self.nav.step(&rows, step);
        cx.notify();
    }

    fn nav_connection(&self) -> Option<ConnectionConfig> {
        let id = self.nav.cursor()?;
        self.connections.iter().find(|c| c.id == id).cloned()
    }

    pub fn set_state(
        &mut self,
        connections: Vec<ConnectionConfig>,
        folders: Vec<ConnectionFolder>,
        selected: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.connections = connections;
        self.folders = folders;
        self.selected = selected;
        cx.notify();
    }

    fn connect(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let bar = state::bar(cx);
        let target = id.clone();
        let task = state::spawn(cx, async move { bar.connect(&target).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |_, window, cx| match result {
                Some(Ok(())) => cx.emit(ConnectionsEvent::Connected(id)),
                Some(Err(error)) => failed(error.message, "errors.couldNotConnect", window, cx),
                None => {}
            });
        })
        .detach();
    }

    fn disconnect(&mut self, id: String, cx: &mut Context<Self>) {
        let bar = state::bar(cx);
        let target = id.clone();
        let task = state::spawn(cx, async move { bar.disconnect(&target).await });
        cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |_, cx| {
                schema::forget(&id, cx);
                cx.emit(ConnectionsEvent::Changed);
            });
        })
        .detach();
    }

    fn delete(&mut self, connection: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        let confirm = Confirm {
            title: t(cx, "dialog.deleteConnectionTitle"),
            description: t_with(cx, "dialog.deleteConnectionDescription", &[("name", &connection.name)]),
            detail: Some(t(cx, "dialog.deleteConnectionDetail")),
            confirm: t(cx, "dialog.deleteConnectionConfirm"),
            danger: true,
        };
        let this = cx.entity().downgrade();
        dialogs::confirm(confirm, window, cx, move |_, cx| {
            let bar = state::bar(cx);
            let id = connection.id.clone();
            let task = state::spawn(cx, async move { bar.delete_connection(&id).await });
            let (this, id) = (this.clone(), connection.id.clone());
            cx.spawn(async move |cx| {
                task.await;
                let _ = this.update(cx, |_, cx| {
                    schema::forget(&id, cx);
                    cx.emit(ConnectionsEvent::Changed);
                });
            })
            .detach();
        });
    }

    // Dropping on a connection moves the dragged one to its index.
    fn reorder(&mut self, from_id: &str, to_id: &str, cx: &mut Context<Self>) {
        let from = self.connections.iter().position(|c| c.id == from_id);
        let to = self.connections.iter().position(|c| c.id == to_id);
        let (Some(from), Some(to)) = (from, to) else { return };
        if from == to {
            return;
        }
        let moved = self.connections.remove(from);
        self.connections.insert(to, moved);
        let ordered: Vec<String> = self.connections.iter().map(|c| c.id.clone()).collect();
        state::bar(cx).reorder_connections(&ordered);
        cx.emit(ConnectionsEvent::Changed);
        cx.notify();
    }

    fn move_to_folder(
        &mut self,
        connection: ConnectionConfig,
        folder_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if connection.folder_id == folder_id {
            return;
        }
        let bar = state::bar(cx);
        let task =
            state::spawn(cx, async move { bar.save_connection(ConnectionConfig { folder_id, ..connection }).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |_, window, cx| {
                if let Some(Err(error)) = result {
                    failed(error.message, "errors.generic", window, cx);
                }
                cx.emit(ConnectionsEvent::Changed);
            });
        })
        .detach();
    }

    fn create_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = Prompt {
            title: t(cx, "dialog.newFolderTitle"),
            description: None,
            label: t(cx, "dialog.folderNameLabel"),
            placeholder: t(cx, "dialog.folderNamePlaceholder"),
            initial: String::new(),
            confirm: t(cx, "common.create"),
        };
        let this = cx.entity().downgrade();
        dialogs::prompt(prompt, window, cx, move |name, _, cx| {
            state::bar(cx).save_folder(ConnectionFolder { id: String::new(), name });
            let _ = this.update(cx, |_, cx| cx.emit(ConnectionsEvent::Changed));
        });
    }

    fn rename_folder(&mut self, folder: ConnectionFolder, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = Prompt {
            title: t(cx, "dialog.renameFolderTitle"),
            description: None,
            label: t(cx, "dialog.folderNameLabel"),
            placeholder: SharedString::default(),
            initial: folder.name.clone(),
            confirm: t(cx, "common.save"),
        };
        let this = cx.entity().downgrade();
        dialogs::prompt(prompt, window, cx, move |name, _, cx| {
            if name != folder.name {
                state::bar(cx).save_folder(ConnectionFolder { id: folder.id.clone(), name });
                let _ = this.update(cx, |_, cx| cx.emit(ConnectionsEvent::Changed));
            }
        });
    }

    fn delete_folder(&mut self, folder: ConnectionFolder, window: &mut Window, cx: &mut Context<Self>) {
        let confirm = Confirm {
            title: t(cx, "dialog.deleteFolderTitle"),
            description: t_with(cx, "dialog.deleteFolderDescription", &[("name", &folder.name)]),
            detail: Some(t(cx, "dialog.deleteFolderDetail")),
            confirm: t(cx, "common.delete"),
            danger: true,
        };
        let this = cx.entity().downgrade();
        dialogs::confirm(confirm, window, cx, move |_, cx| {
            state::bar(cx).delete_folder(&folder.id);
            let _ = this.update(cx, |_, cx| cx.emit(ConnectionsEvent::Changed));
        });
    }

    fn toggle_folder(&mut self, id: &str, cx: &mut Context<Self>) {
        let collapsed = self.collapsed.get(id) == Some(&true);
        self.collapsed.insert(id.to_string(), !collapsed);
        set_setting_json(cx, COLLAPSED_KEY, &self.collapsed);
        cx.notify();
    }

    fn connection_menu(
        &self,
        connection: &ConnectionConfig,
        connected: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let (connection, folders) = (connection.clone(), self.folders.clone());
        move |menu, _, cx| {
            let with = |f: PanelAction| {
                let this = this.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |panel, cx| f(panel, window, cx));
                }
            };
            let (id, edit, duplicate, delete) =
                (connection.id.clone(), connection.clone(), connection.clone(), connection.clone());
            let duplicate_name = t_with(cx, "sidebar.duplicateSuffix", &[("name", &connection.name)]).to_string();
            let mut menu = menu
                .item(
                    PopupMenuItem::new(t(cx, if connected { "tooltip.disconnect" } else { "tooltip.connect" }))
                        .on_click(with(Box::new(move |panel, window, cx| {
                            if connected {
                                panel.disconnect(id.clone(), cx)
                            } else {
                                panel.connect(id.clone(), window, cx)
                            }
                        }))),
                )
                .item(
                    PopupMenuItem::new(t(cx, "common.edit")).on_click(with(Box::new(move |_, _, cx| {
                        cx.emit(ConnectionsEvent::Edit(Box::new(edit.clone())))
                    }))),
                )
                .item(PopupMenuItem::new(t(cx, "sidebar.duplicate")).on_click(with(Box::new(move |_, _, cx| {
                    let copy =
                        ConnectionConfig { id: String::new(), name: duplicate_name.clone(), ..duplicate.clone() };
                    cx.emit(ConnectionsEvent::Edit(Box::new(copy)))
                }))));
            if !folders.is_empty() {
                menu = menu.separator();
                for folder in &folders {
                    let (target, folder_id) = (connection.clone(), folder.id.clone());
                    menu = menu.item(
                        PopupMenuItem::new(t_with(cx, "sidebar.moveToFolder", &[("name", &folder.name)]))
                            .disabled(connection.folder_id == folder.id)
                            .on_click(with(Box::new(move |panel, window, cx| {
                                panel.move_to_folder(target.clone(), folder_id.clone(), window, cx)
                            }))),
                    );
                }
                if !connection.folder_id.is_empty() {
                    let target = connection.clone();
                    menu = menu.item(PopupMenuItem::new(t(cx, "sidebar.removeFromFolder")).on_click(with(Box::new(
                        move |panel, window, cx| panel.move_to_folder(target.clone(), String::new(), window, cx),
                    ))));
                }
            }
            menu.separator().item(
                PopupMenuItem::new(t(cx, "common.delete"))
                    .on_click(with(Box::new(move |panel, window, cx| panel.delete(delete.clone(), window, cx)))),
            )
        }
    }

    fn render_connection(&self, connection: &ConnectionConfig, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let ring = self.nav.ringed(&connection.id, window);
        let connected = state::bar(cx).is_connected(&connection.id);
        let selected = self.selected.as_deref() == Some(connection.id.as_str());
        let color = parse_color(&connection.color).unwrap_or(theme.primary);
        let menu = self.connection_menu(connection, connected, cx);
        let (activate, toggle, edit, delete) =
            (connection.id.clone(), connection.id.clone(), connection.clone(), connection.clone());
        let group: SharedString = format!("conn-{}", connection.id).into();
        h_flex()
            .id(SharedString::from(format!("connection-{}", connection.id)))
            .group(group.clone())
            .relative()
            .w_full()
            .py(rems(0.538))
            .px(rems(0.615))
            .gap(rems(0.462))
            .rounded(RADIUS)
            .text_size(TEXT_BASE)
            .cursor_pointer()
            .when(selected, |el| el.bg(theme.sidebar_accent))
            .hover(|s| s.bg(theme.sidebar_accent))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(rems(0.385))
                    .bottom(rems(0.385))
                    .w(rems(0.231))
                    .rounded_r(rems(0.154))
                    .bg(if connected { theme.success } else { theme.danger }),
            )
            .child(Icon::new(Lucide::Database).size(ICON_SM).text_color(color))
            .child(
                v_flex().flex_1().min_w_0().gap(px(1.)).child(div().truncate().child(connection.name.clone())).child(
                    div()
                        .truncate()
                        .text_size(TEXT_XS)
                        .text_color(theme.muted_foreground)
                        .child(connection_subtitle(connection)),
                ),
            )
            .child(
                list_nav::hover_actions(group, rems(0.615), cx)
                    .pr(self.nav.bar_clearance(rems(0.615), window))
                    .child(
                        Button::new(SharedString::from(format!("connect-{}", connection.id)))
                            .small()
                            .debug_selector({
                                let id = connection.id.clone();
                                move || format!("connect-{id}")
                            })
                            .icon(Icon::new(if connected { Lucide::Unplug } else { Lucide::Plug }))
                            .tooltip(t(cx, if connected { "tooltip.disconnect" } else { "tooltip.connect" }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if connected {
                                    this.disconnect(toggle.clone(), cx)
                                } else {
                                    this.connect(toggle.clone(), window, cx)
                                }
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("edit-{}", connection.id)))
                            .small()
                            .debug_selector({
                                let id = connection.id.clone();
                                move || format!("edit-{id}")
                            })
                            .icon(Icon::new(Lucide::SquarePen))
                            .tooltip(t(cx, "common.edit"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(ConnectionsEvent::Edit(Box::new(edit.clone())));
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("delete-{}", connection.id)))
                            .small()
                            .danger()
                            .outline()
                            .debug_selector({
                                let id = connection.id.clone();
                                move || format!("delete-{id}")
                            })
                            .icon(Icon::new(Lucide::Trash))
                            .tooltip(t(cx, "common.delete"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.delete(delete.clone(), window, cx);
                            })),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = connection.id.clone();
                    move |this, _, window, cx| this.nav.pick(&id, window, cx)
                }),
            )
            .when(ring, |el| list_nav::ring(el, cx))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(ConnectionsEvent::Activate(activate.clone()))))
            .debug_selector({
                let id = connection.id.clone();
                move || format!("connection-row-{id}")
            })
            .on_drag(
                ConnectionDrag { id: connection.id.clone(), name: connection.name.clone().into() },
                |drag, _, _, cx| cx.new(|_| drag.clone()),
            )
            .drag_over::<ConnectionDrag>(|style, _, _, cx| drop_ring(style, cx))
            .on_drop(cx.listener({
                let target = connection.id.clone();
                move |this, drag: &ConnectionDrag, _, cx| this.reorder(&drag.id, &target, cx)
            }))
            .context_menu(menu)
            .into_any_element()
    }

    fn render_folder(&self, folder: &ConnectionFolder, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let collapsed = self.collapsed.get(&folder.id) == Some(&true);
        let members: Vec<ConnectionConfig> =
            self.connections.iter().filter(|c| c.folder_id == folder.id).cloned().collect();
        let this = cx.entity().downgrade();
        let menu_folder = folder.clone();
        let toggle = folder.id.clone();
        let header = h_flex()
            .id(SharedString::from(format!("folder-{}", folder.id)))
            .mt(rems(0.154))
            .py(rems(0.385))
            .px(rems(0.462))
            .gap(rems(0.462))
            .rounded(RADIUS)
            .cursor_pointer()
            .text_size(TEXT_XS)
            .font_semibold()
            .text_color(theme.muted_foreground)
            .hover(|s| s.bg(theme.sidebar_accent).text_color(theme.foreground))
            .child(Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }).size(ICON_XS))
            .child(Icon::new(IconName::Folder).size(ICON_XS))
            .child(div().flex_1().truncate().child(folder.name.clone()))
            .child(div().child(members.len().to_string()))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_folder(&toggle, cx)))
            .debug_selector({
                let id = folder.id.clone();
                move || format!("folder-row-{id}")
            })
            .drag_over::<ConnectionDrag>(|style, _, _, cx| drop_ring(style, cx))
            .on_drop(cx.listener({
                let folder_id = folder.id.clone();
                move |this, drag: &ConnectionDrag, window, cx| {
                    if let Some(connection) = this.connections.iter().find(|c| c.id == drag.id).cloned() {
                        this.move_to_folder(connection, folder_id.clone(), window, cx);
                    }
                }
            }))
            .context_menu(move |menu, _, cx| {
                let (rename, delete) = (menu_folder.clone(), menu_folder.clone());
                let (this_rename, this_delete) = (this.clone(), this.clone());
                menu.item(PopupMenuItem::new(t(cx, "sidebar.renameFolder")).on_click(move |_, window, cx| {
                    let _ = this_rename.update(cx, |panel, cx| panel.rename_folder(rename.clone(), window, cx));
                }))
                .item(PopupMenuItem::new(t(cx, "common.delete")).on_click(move |_, window, cx| {
                    let _ = this_delete.update(cx, |panel, cx| panel.delete_folder(delete.clone(), window, cx));
                }))
            });
        let body = (!collapsed).then(|| {
            if members.is_empty() {
                div()
                    .py(rems(0.385))
                    .px(rems(0.615))
                    .ml(rems(0.615))
                    .text_size(TEXT_XS)
                    .text_color(theme.muted_foreground)
                    .child(t(cx, "sidebar.folderEmpty"))
                    .into_any_element()
            } else {
                let rows: Vec<AnyElement> = members.iter().map(|c| self.render_connection(c, window, cx)).collect();
                v_flex().pl(rems(0.615)).children(rows).into_any_element()
            }
        });
        v_flex().child(header).children(body).into_any_element()
    }
}

impl Render for ConnectionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toolbar = h_flex()
            .gap(rems(0.308))
            .mb(rems(0.769))
            .child(
                Button::new("new-connection")
                    .debug_selector(|| "new-connection".into())
                    .flex_1()
                    .icon(Icon::new(IconName::Plus))
                    .label(t(cx, "sidebar.newConnection"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ConnectionsEvent::Edit(Box::default())))),
            )
            .child(
                Button::new("new-folder")
                    .icon(Icon::new(Lucide::FolderPlus))
                    .tooltip(t(cx, "sidebar.newFolder"))
                    .on_click(cx.listener(|this, _, window, cx| this.create_folder(window, cx))),
            );
        let folder_ids: Vec<&str> = self.folders.iter().map(|f| f.id.as_str()).collect();
        let loose: Vec<ConnectionConfig> = self
            .connections
            .iter()
            .filter(|c| c.folder_id.is_empty() || !folder_ids.contains(&c.folder_id.as_str()))
            .cloned()
            .collect();
        let body = if self.connections.is_empty() {
            form::empty_state(Some(Icon::new(Lucide::Database)), t(cx, "sidebar.noConnectionsYet"), cx)
                .into_any_element()
        } else {
            let mut rows: Vec<AnyElement> = loose.iter().map(|c| self.render_connection(c, window, cx)).collect();
            let folders = self.folders.clone();
            rows.extend(folders.iter().map(|f| self.render_folder(f, window, cx)));
            let list = v_flex()
                .id("connection-list")
                .debug_selector(|| "connection-list".into())
                .key_context(list_nav::CONTEXT)
                .track_focus(&self.nav.focus)
                .track_scroll(&self.nav.scroll)
                .max_h(px(420.))
                .overflow_y_scroll()
                .on_action(cx.listener(|this, _: &NavUp, _, cx| this.step(-1, cx)))
                .on_action(cx.listener(|this, _: &NavDown, _, cx| this.step(1, cx)))
                .on_action(cx.listener(|this, _: &NavFirst, _, cx| this.step(isize::MIN, cx)))
                .on_action(cx.listener(|this, _: &NavLast, _, cx| this.step(isize::MAX, cx)))
                .on_action(cx.listener(|this, _: &NavOpen, _, cx| {
                    if let Some(connection) = this.nav_connection() {
                        cx.emit(ConnectionsEvent::Activate(connection.id));
                    }
                }))
                .on_action(cx.listener(|this, _: &NavDelete, window, cx| {
                    if let Some(connection) = this.nav_connection() {
                        this.delete(connection, window, cx);
                    }
                }))
                .children(rows);
            v_flex()
                .relative()
                .min_h_0()
                .child(list)
                .vertical_scrollbar(&self.nav.scroll)
                .scrollbars_on_hover()
                .into_any_element()
        };
        // Shrinks to fit the switcher menu, so the list scrolls and the toolbar stays.
        v_flex().w_full().min_h_0().child(toolbar).child(body)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use barsql_app::ConnectionFolder;
    use barsql_core::{ConnectionConfig, DriverType};
    use gpui_kit::TestAppContext;

    use super::{ConnectionsEvent, ConnectionsPanel, connection_subtitle, parse_color};
    use crate::test_support::{Env, settle};

    #[gpui_kit::test]
    fn connections_drag_into_order_and_folders(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let folder = env.bar.save_folder(ConnectionFolder { id: String::new(), name: "Prod".into() });
        let save = |name: &str| {
            let config = ConnectionConfig {
                name: name.into(),
                driver: DriverType::Sqlite,
                file_path: format!("/tmp/{name}.db"),
                ..Default::default()
            };
            env.runtime.block_on(env.bar.save_connection(config)).unwrap().id
        };
        let (b, c) = (save("B"), save("C"));
        let a = env.connection.id.clone();
        let (panel, cx) = cx.add_window_view(|_, cx| ConnectionsPanel::new(cx));
        let load = |cx: &mut gpui_kit::VisualTestContext| {
            let (connections, folders) = (env.bar.list_connections(), env.bar.list_folders());
            panel.update(cx, |panel, cx| panel.set_state(connections, folders, None, cx));
            cx.run_until_parked();
        };
        load(cx);
        let drag = |cx: &mut gpui_kit::VisualTestContext, from: String, to: String| {
            let from: &'static str = Box::leak(from.into_boxed_str());
            let to: &'static str = Box::leak(to.into_boxed_str());
            let (start, end) = (cx.debug_bounds(from).unwrap().center(), cx.debug_bounds(to).unwrap().center());
            let (left, none) = (gpui_kit::MouseButton::Left, gpui_kit::Modifiers::default());
            cx.simulate_mouse_down(start, left, none);
            cx.simulate_mouse_move(start + gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(6.)), left, none);
            cx.simulate_mouse_move(end, left, none);
            cx.simulate_mouse_up(end, left, none);
            cx.run_until_parked();
        };
        let order = || env.bar.list_connections().into_iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(order(), [a.clone(), b.clone(), c.clone()]);
        drag(cx, format!("connection-row-{a}"), format!("connection-row-{c}"));
        assert_eq!(order(), [b.clone(), c.clone(), a.clone()]);

        load(cx);
        drag(cx, format!("connection-row-{b}"), format!("folder-row-{}", folder.id));
        settle(cx, |_| env.bar.list_connections().iter().any(|conn| conn.id == b && conn.folder_id == folder.id));
    }

    #[gpui_kit::test]
    fn moving_connecting_and_disconnecting_report_back(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let folder = env.bar.save_folder(ConnectionFolder { id: String::new(), name: "Prod".into() });
        let (panel, cx) = cx.add_window_view(|_, cx| ConnectionsPanel::new(cx));
        let (connections, folders) = (env.bar.list_connections(), env.bar.list_folders());
        panel.update(cx, |panel, cx| panel.set_state(connections, folders, None, cx));
        let events = Rc::new(RefCell::new(Vec::new()));
        let seen = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &ConnectionsEvent, _| {
                seen.borrow_mut().push(match event {
                    ConnectionsEvent::Activate(id) => format!("activate {id}"),
                    ConnectionsEvent::Connected(id) => format!("connected {id}"),
                    ConnectionsEvent::Edit(_) => "edit".into(),
                    ConnectionsEvent::Changed => "changed".into(),
                })
            })
            .detach()
        });
        let connection = env.connection.clone();
        let id = connection.id.clone();
        panel.update_in(cx, |panel, window, cx| panel.move_to_folder(connection, folder.id.clone(), window, cx));
        settle(cx, |_| events.borrow().contains(&"changed".to_string()));
        assert_eq!(env.bar.list_connections()[0].folder_id, folder.id);

        panel.update_in(cx, |panel, window, cx| panel.connect(id.clone(), window, cx));
        settle(cx, |_| events.borrow().contains(&format!("connected {id}")));
        assert!(env.bar.is_connected(&id));
        panel.update(cx, |panel, cx| panel.disconnect(id.clone(), cx));
        settle(cx, |_| !env.bar.is_connected(&id));
    }

    #[test]
    fn subtitles_name_the_file_or_the_server() {
        let sqlite =
            ConnectionConfig { driver: DriverType::Sqlite, file_path: "/data/notes.db".into(), ..Default::default() };
        assert_eq!(connection_subtitle(&sqlite), "sqlite · notes.db");
        let windows = ConnectionConfig {
            driver: DriverType::Sqlite,
            file_path: r"C:\data\app.sqlite".into(),
            ..Default::default()
        };
        assert_eq!(connection_subtitle(&windows), "sqlite · app.sqlite");
        assert_eq!(
            connection_subtitle(&ConnectionConfig { driver: DriverType::Sqlite, ..Default::default() }),
            "sqlite"
        );
        let pg = ConnectionConfig {
            driver: DriverType::Postgres,
            host: "db.local".into(),
            database: "shop".into(),
            ..Default::default()
        };
        assert_eq!(connection_subtitle(&pg), "postgres · db.local/shop");
        let bare = ConnectionConfig { driver: DriverType::MySql, ..Default::default() };
        assert_eq!(connection_subtitle(&bare), "mysql");
        assert!(parse_color("#22c55e").is_some());
        assert!(parse_color("green").is_none());
    }
}
