use std::rc::Rc;

use barsql_app::path_defaults;
use barsql_core::{ConnectionConfig, DriverType, SshConfig};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::connections_panel::parse_color;
use crate::file_dialogs;
use crate::form;
use crate::i18n::{t, t_with};
use crate::modal::{self, Modal};
use crate::state;
use crate::toast;

pub const DEFAULT_COLOR: &str = "#3b82f6";
// 500px. 480px fits two rows of 15 swatches with 2px to spare, and device-pixel rounding eats that at some zooms
// and display scales, like 125% at the default zoom.
const WIDTH: f32 = 38.462;
const DEFAULT_SSH_PORT: i64 = 22;
// Tailwind 500, then 700.
const COLORS: [&str; 30] = [
    "#ef4444", "#f97316", "#f59e0b", "#eab308", "#84cc16", "#22c55e", "#14b8a6", "#06b6d4", "#0ea5e9", "#3b82f6",
    "#6366f1", "#8b5cf6", "#a855f7", "#ec4899", "#64748b", "#b91c1c", "#c2410c", "#b45309", "#a16207", "#4d7c0f",
    "#15803d", "#0f766e", "#0e7490", "#0369a1", "#1d4ed8", "#4338ca", "#6d28d9", "#7e22ce", "#be185d", "#334155",
];

type OnSaved = Rc<dyn Fn(ConnectionConfig, &mut Window, &mut App)>;

pub fn new_connection() -> ConnectionConfig {
    ConnectionConfig {
        driver: DriverType::Sqlite,
        color: DEFAULT_COLOR.into(),
        host: "localhost".into(),
        port: 5432,
        username: "postgres".into(),
        ssl_mode: "disable".into(),
        schema: "public".into(),
        ..Default::default()
    }
}

fn network(driver: &DriverType) -> bool {
    matches!(driver, DriverType::Postgres | DriverType::MySql)
}

fn database_placeholder(driver: &DriverType) -> &'static str {
    if *driver == DriverType::MySql { "connection.databasePlaceholderMysql" } else { "connection.databasePlaceholder" }
}

fn default_port(driver: &DriverType) -> i64 {
    if *driver == DriverType::MySql { 3306 } else { 5432 }
}

// Leading digits only. Zero or no digits gives the fallback.
fn parse_port(value: &str, fallback: i64) -> i64 {
    let digits: String = value.trim().chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<i64>().ok().filter(|port| *port != 0).unwrap_or(fallback)
}

fn input(
    value: String,
    placeholder: SharedString,
    masked: bool,
    window: &mut Window,
    cx: &mut Context<ConnectionForm>,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).default_value(value).placeholder(placeholder).masked(masked))
}

struct Fields {
    name: Entity<InputState>,
    file_path: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    database: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    schema: Entity<InputState>,
    ssh_host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    ssh_username: Entity<InputState>,
    ssh_password: Entity<InputState>,
    ssh_key_path: Entity<InputState>,
    ssh_passphrase: Entity<InputState>,
    ssh_known_hosts: Entity<InputState>,
}

pub struct ConnectionForm {
    // Keeps the id, folder and unknown fields for the save.
    original: ConnectionConfig,
    driver: DriverType,
    read_only: bool,
    color: String,
    ssl_mode: String,
    ssh_enabled: bool,
    ssh_auth: String,
    ignore_host_key: bool,
    fields: Fields,
    error: Option<SharedString>,
    testing: bool,
    saving: bool,
    // Last listed databases and the server they came from.
    databases: Option<(String, Vec<String>)>,
    on_saved: OnSaved,
    scroll: ScrollHandle,
}

impl ConnectionForm {
    fn new(config: ConnectionConfig, on_saved: OnSaved, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paths = path_defaults();
        let file_placeholder =
            if cfg!(windows) { "connection.filePlaceholderWindows" } else { "connection.filePlaceholderUnix" };
        let port = if config.port == 0 { default_port(&config.driver) } else { config.port };
        let schema = match config.driver {
            DriverType::MySql => {
                if config.schema.is_empty() {
                    config.database.clone()
                } else {
                    config.schema.clone()
                }
            }
            _ => {
                if config.schema.is_empty() {
                    "public".into()
                } else {
                    config.schema.clone()
                }
            }
        };
        let ssh = &config.ssh;
        let ssh_port = if ssh.port == 0 { DEFAULT_SSH_PORT } else { ssh.port };
        let fields = Fields {
            name: input(config.name.clone(), t(cx, "connection.namePlaceholder"), false, window, cx),
            file_path: input(config.file_path.clone(), t(cx, file_placeholder), false, window, cx),
            host: input(config.host.clone(), SharedString::default(), false, window, cx),
            port: input(port.to_string(), SharedString::default(), false, window, cx),
            database: input(config.database.clone(), t(cx, database_placeholder(&config.driver)), false, window, cx),
            username: input(config.username.clone(), SharedString::default(), false, window, cx),
            password: input(config.password.clone(), SharedString::default(), true, window, cx),
            schema: input(schema, SharedString::default(), false, window, cx),
            ssh_host: input(ssh.host.clone(), t(cx, "connection.sshHostPlaceholder"), false, window, cx),
            ssh_port: input(ssh_port.to_string(), SharedString::default(), false, window, cx),
            ssh_username: input(ssh.username.clone(), SharedString::default(), false, window, cx),
            ssh_password: input(ssh.password.clone(), SharedString::default(), true, window, cx),
            ssh_key_path: input(ssh.key_path.clone(), paths.ssh_key.clone().into(), false, window, cx),
            ssh_passphrase: input(
                ssh.passphrase.clone(),
                t(cx, "connection.sshPassphrasePlaceholder"),
                true,
                window,
                cx,
            ),
            ssh_known_hosts: input(ssh.known_hosts.clone(), paths.ssh_known_hosts.clone().into(), false, window, cx),
        };
        Self {
            driver: config.driver.clone(),
            read_only: config.read_only,
            color: if config.color.is_empty() { DEFAULT_COLOR.into() } else { config.color.clone() },
            ssl_mode: if config.ssl_mode.is_empty() { "disable".into() } else { config.ssl_mode.clone() },
            ssh_enabled: ssh.enabled,
            ssh_auth: if ssh.auth.is_empty() { "key".into() } else { ssh.auth.clone() },
            ignore_host_key: ssh.ignore_host_key,
            original: config,
            fields,
            error: None,
            testing: false,
            saving: false,
            databases: None,
            on_saved,
            scroll: ScrollHandle::new(),
        }
    }

    fn value(state: &Entity<InputState>, cx: &App) -> String {
        state.read(cx).value().to_string()
    }

    fn set(state: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut App) {
        let value = value.to_string();
        state.update(cx, |state, cx| state.set_value(value, window, cx));
    }

    fn set_driver(&mut self, driver: DriverType, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = match driver {
            DriverType::Postgres => Some(("5432", "postgres", "public")),
            DriverType::MySql => Some(("3306", "root", "")),
            _ => None,
        };
        if let Some((port, username, schema)) = defaults {
            Self::set(&self.fields.port, port, window, cx);
            Self::set(&self.fields.username, username, window, cx);
            Self::set(&self.fields.schema, schema, window, cx);
            self.ssl_mode = "disable".into();
        }
        let placeholder = t(cx, database_placeholder(&driver));
        self.fields.database.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
        self.driver = driver;
        cx.notify();
    }

    // Raw form values, which is what Test sends.
    fn config(&self, cx: &App) -> ConnectionConfig {
        let f = &self.fields;
        let value = |state: &Entity<InputState>| Self::value(state, cx);
        ConnectionConfig {
            name: value(&f.name),
            driver: self.driver.clone(),
            color: self.color.clone(),
            file_path: value(&f.file_path),
            host: value(&f.host),
            port: parse_port(&value(&f.port), default_port(&self.driver)),
            database: value(&f.database),
            username: value(&f.username),
            password: value(&f.password),
            ssl_mode: self.ssl_mode.clone(),
            schema: value(&f.schema),
            read_only: self.read_only,
            ssh: SshConfig {
                enabled: self.ssh_enabled,
                host: value(&f.ssh_host),
                port: parse_port(&value(&f.ssh_port), DEFAULT_SSH_PORT),
                username: value(&f.ssh_username),
                auth: self.ssh_auth.clone(),
                password: value(&f.ssh_password),
                key_path: value(&f.ssh_key_path),
                passphrase: value(&f.ssh_passphrase),
                known_hosts: value(&f.ssh_known_hosts),
                ignore_host_key: self.ignore_host_key,
                ..self.original.ssh.clone()
            },
            ..self.original.clone()
        }
    }

    // Blanks the fields that don't pick a server, so editing them keeps the cached database list.
    fn server_key(&self, cx: &App) -> String {
        ConnectionConfig {
            name: String::new(),
            color: String::new(),
            database: String::new(),
            schema: String::new(),
            read_only: false,
            ..self.config(cx)
        }
        .fingerprint()
    }

    // MySQL's browse database follows the database field while the two are equal.
    fn pick_database(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let previous = Self::value(&self.fields.database, cx);
        Self::set(&self.fields.database, name, window, cx);
        if self.driver == DriverType::MySql && Self::value(&self.fields.schema, cx) == previous {
            Self::set(&self.fields.schema, name, window, cx);
        }
        cx.notify();
    }

    // Lists databases with the credentials as entered when the menu opens. A failure closes the menu with a toast.
    fn database_picker(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let form = cx.entity().downgrade();
        form::filter_button("conn-database-list", Icon::new(Lucide::Database))
            .debug_selector(|| "conn-database-list".into())
            .tooltip(t(cx, "connection.showDatabases"))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                let Some(form) = form.upgrade() else { return menu };
                let key = form.read(cx).server_key(cx);
                let cached =
                    form.read(cx).databases.as_ref().filter(|(server, _)| *server == key).map(|(_, l)| l.clone());
                if let Some(databases) = cached {
                    return database_items(menu, &databases, &form, cx);
                }
                let bar = state::bar(cx);
                let config = form.read(cx).config(cx);
                let task = state::spawn(cx, async move { bar.list_databases(config).await });
                let form = form.downgrade();
                cx.spawn_in(window, async move |menu, cx| {
                    let result = task.await;
                    menu.update_in(cx, |menu, window, cx| match result {
                        Some(Ok(databases)) => {
                            let Some(form) = form.upgrade() else { return };
                            form.update(cx, |form, _| form.databases = Some((key, databases.clone())));
                            menu.rebuild(window, cx, |menu, _, cx| database_items(menu, &databases, &form, cx));
                        }
                        Some(Err(error)) => {
                            toast::error_in(t(cx, "connection.databasesFailed"), error.message, cx);
                            cx.emit(DismissEvent);
                        }
                        None => {}
                    })
                    .ok();
                })
                .detach();
                menu.item(PopupMenuItem::new(t(cx, "connection.loadingDatabases")).disabled(true))
            })
    }

    fn validate(&self, config: &ConnectionConfig) -> Option<&'static str> {
        if config.name.trim().is_empty() {
            Some("connection.nameRequired")
        } else if config.driver == DriverType::Sqlite && config.file_path.trim().is_empty() {
            Some("connection.fileRequired")
        } else if network(&config.driver) && config.database.trim().is_empty() {
            Some("connection.dbRequired")
        } else {
            None
        }
    }

    fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.testing = true;
        let config = self.config(cx);
        let bar = state::bar(cx);
        let task = state::spawn(cx, async move { bar.test_connection(config).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.testing = false;
                match result {
                    Some(Ok(())) => toast::success(t(cx, "toast.connectionSuccess"), cx),
                    Some(Err(error)) => toast::error(error.message, cx),
                    None => return,
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let mut config = self.config(cx);
        if let Some(key) = self.validate(&config) {
            self.error = Some(t(cx, key));
            cx.notify();
            return;
        }
        self.error = None;
        self.saving = true;
        config.database = config.database.trim().to_string();
        config.host = config.host.trim().to_string();
        config.schema = match config.driver {
            DriverType::Postgres => {
                Some(config.schema.trim()).filter(|s| !s.is_empty()).unwrap_or("public").to_string()
            }
            DriverType::MySql => {
                Some(config.schema.trim()).filter(|s| !s.is_empty()).unwrap_or(config.database.as_str()).to_string()
            }
            _ => String::new(),
        };
        let bar = state::bar(cx);
        let task = state::spawn(cx, async move { bar.save_connection(config).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Some(Ok(saved)) => {
                        window.close_dialog(cx);
                        (this.on_saved.clone())(saved, window, cx);
                    }
                    Some(Err(error)) => this.error = Some(error.message.into()),
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn browse(&mut self, target: Browse, window: &mut Window, cx: &mut Context<Self>) {
        let pick = match target {
            Browse::Sqlite => file_dialogs::pick_sqlite_file(cx),
            Browse::SshKey => file_dialogs::pick_ssh_file("Select SSH private key", cx),
            Browse::KnownHosts => file_dialogs::pick_ssh_file("Select known_hosts file", cx),
        };
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = pick.await else { return };
            let path = path.display().to_string();
            let _ = this.update_in(cx, |this, window, cx| match target {
                Browse::Sqlite => {
                    Self::set(&this.fields.file_path, &path, window, cx);
                    if Self::value(&this.fields.name, cx).is_empty() {
                        let name =
                            path.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or("SQLite").to_string();
                        Self::set(&this.fields.name, &name, window, cx);
                    }
                }
                Browse::SshKey => Self::set(&this.fields.ssh_key_path, &path, window, cx),
                Browse::KnownHosts => Self::set(&this.fields.ssh_known_hosts, &path, window, cx),
            });
        })
        .detach();
    }

    fn select(
        &self,
        id: &'static str,
        current: SharedString,
        options: Vec<(SharedString, SharedString)>,
        pick: fn(&mut Self, SharedString, &mut Window, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let this = cx.entity().downgrade();
        let label =
            options.iter().find(|(value, _)| *value == current).map_or(current.clone(), |(_, label)| label.clone());
        form::select(id, label, cx)
            .debug_selector(move || id.into())
            .dropdown_menu(move |mut menu, _, _| {
                for (value, label) in &options {
                    let (this, value) = (this.clone(), value.clone());
                    menu = menu.item(PopupMenuItem::new(label.clone()).checked(value == current).on_click(
                        move |_, window, cx| {
                            let _ = this.update(cx, |form, cx| pick(form, value.clone(), window, cx));
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    }

    fn toggle(
        &self,
        id: &'static str,
        checked: bool,
        label: &str,
        hint: &str,
        flip: fn(&mut Self),
        cx: &mut Context<Self>,
    ) -> Div {
        v_flex()
            .child(form::checkbox(id, checked, t(cx, label), false, cx).debug_selector(move || id.into()).on_click(
                cx.listener(move |form, _, _, cx| {
                    flip(form);
                    cx.notify();
                }),
            ))
            .child(form::hint(t(cx, hint), cx).pl(rems(2.)))
    }

    fn browse_row(
        &self,
        input: &Entity<InputState>,
        id: &'static str,
        target: Browse,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        h_flex().gap(rems(0.615)).child(form::input(input, window, cx).flex_1()).child(
            Button::new(id)
                .debug_selector(move || id.into())
                .label(t(cx, "common.browse"))
                .on_click(cx.listener(move |form, _, window, cx| form.browse(target, window, cx))),
        )
    }
}

fn database_items(mut menu: PopupMenu, databases: &[String], form: &Entity<ConnectionForm>, cx: &App) -> PopupMenu {
    if databases.is_empty() {
        return menu.item(PopupMenuItem::new(t(cx, "connection.noDatabases")).disabled(true));
    }
    let current = ConnectionForm::value(&form.read(cx).fields.database, cx);
    for name in databases {
        let (form, pick) = (form.downgrade(), name.clone());
        menu = menu.item(PopupMenuItem::new(name.clone()).checked(*name == current).on_click(move |_, window, cx| {
            let _ = form.update(cx, |form, cx| form.pick_database(&pick, window, cx));
        }));
    }
    menu.scrollable(true)
}

// Last opened dialog, for the scenario tests.
#[cfg(all(test, feature = "e2e"))]
pub(crate) struct Opened(pub WeakEntity<ConnectionForm>);

#[cfg(all(test, feature = "e2e"))]
impl Global for Opened {}

#[cfg(all(test, feature = "e2e"))]
impl ConnectionForm {
    pub(crate) fn databases(&self) -> Option<&[String]> {
        self.databases.as_ref().map(|(_, databases)| databases.as_slice())
    }

    pub(crate) fn database(&self, cx: &App) -> String {
        Self::value(&self.fields.database, cx)
    }
}

#[derive(Clone, Copy)]
enum Browse {
    Sqlite,
    SshKey,
    KnownHosts,
}

// Gives scenario tests a selector to click into.
fn labelled(selector: &'static str, input: Input) -> Div {
    div().debug_selector(move || selector.into()).child(input)
}

impl Render for ConnectionForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let f = &self.fields;
        let mysql = self.driver == DriverType::MySql;
        let drivers = vec![
            ("sqlite".into(), t(cx, "connection.sqlite")),
            ("postgres".into(), t(cx, "connection.postgres")),
            ("mysql".into(), t(cx, "connection.mysql")),
        ];
        let driver_select = self.select(
            "conn-driver",
            self.driver.to_string().into(),
            drivers,
            |form, value, window, cx| form.set_driver(DriverType::parse(&value), window, cx),
            cx,
        );
        let swatches = h_flex().flex_wrap().gap(rems(0.462)).children(COLORS.iter().map(|color| {
            let selected = self.color == *color;
            let value = color.to_string();
            div()
                .id(SharedString::from(format!("swatch-{color}")))
                .debug_selector(|| format!("swatch-{color}"))
                .size(rems(1.846))
                .rounded_full()
                .cursor_pointer()
                .bg(parse_color(color).unwrap_or(theme.primary))
                .border_2()
                .border_color(if selected { theme.background } else { transparent_black() })
                .when(selected, |el| {
                    el.shadow(vec![BoxShadow {
                        color: theme.primary,
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(2.),
                        inset: false,
                    }])
                })
                .on_click(cx.listener(move |form, _, _, cx| {
                    form.color = value.clone();
                    cx.notify();
                }))
        }));
        let read_only = self.toggle(
            "conn-read-only",
            self.read_only,
            "connection.readOnly",
            "connection.readOnlyHint",
            |form| form.read_only = !form.read_only,
            cx,
        );
        let mut body = modal::body()
            .child(form::group(t(cx, "connection.name"), labelled("conn-name", form::input(&f.name, window, cx)), cx))
            .child(form::group(t(cx, "connection.driver"), driver_select, cx))
            .child(read_only)
            .child(form::group(t(cx, "connection.tabColor"), swatches, cx));
        if self.driver == DriverType::Sqlite {
            let file_row = self.browse_row(&f.file_path, "browse-sqlite", Browse::Sqlite, window, cx);
            body = body.child(form::group(t(cx, "connection.file"), file_row, cx));
        } else {
            let ssl = vec![
                ("disable".into(), t(cx, "connection.sslDisable")),
                ("require".into(), t(cx, "connection.sslRequire")),
                ("verify-full".into(), t(cx, "connection.sslVerifyFull")),
            ];
            let ssl_select = self.select(
                "conn-ssl",
                self.ssl_mode.clone().into(),
                ssl,
                |form, value, _, cx| {
                    form.ssl_mode = value.to_string();
                    cx.notify();
                },
                cx,
            );
            let database_hint = if mysql { "connection.databaseHintMysql" } else { "connection.databaseHint" };
            let schema_label = if mysql { "connection.defaultSchemaMysql" } else { "connection.defaultSchema" };
            body = body
                .child(form::row(
                    form::group(t(cx, "connection.host"), labelled("conn-host", form::input(&f.host, window, cx)), cx),
                    form::group(t(cx, "connection.port"), labelled("conn-port", form::input(&f.port, window, cx)), cx),
                ))
                .child(
                    form::group(
                        t(cx, "connection.databaseName"),
                        h_flex()
                            .gap(rems(0.615))
                            .child(labelled("conn-database", form::input(&f.database, window, cx)).flex_1())
                            .child(self.database_picker(cx)),
                        cx,
                    )
                    .child(form::hint(t(cx, database_hint), cx)),
                )
                .child(form::row(
                    form::group(
                        t(cx, "connection.username"),
                        labelled("conn-username", form::input(&f.username, window, cx)),
                        cx,
                    ),
                    form::group(
                        t(cx, "connection.password"),
                        labelled("conn-password", form::input(&f.password, window, cx).mask_toggle()),
                        cx,
                    ),
                ))
                .child(form::row(
                    form::group(t(cx, "connection.sslMode"), ssl_select, cx),
                    form::group(t(cx, schema_label), form::input(&f.schema, window, cx), cx)
                        .when(mysql, |el| el.child(form::hint(t(cx, "connection.defaultSchemaMysqlHint"), cx))),
                ))
                .child(self.ssh_section(window, cx));
        }
        let body = body.children(self.error.clone().map(|error| form::error(error, cx)));
        let (testing, saving) = (self.testing, self.saving);
        modal::scroll_content().child(modal::scroll_body(&self.scroll, body)).child(
            modal::footer(cx)
                .child(
                    Button::new("connection-cancel")
                        .label(t(cx, "common.cancel"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("connection-test")
                        .debug_selector(|| "connection-test".into())
                        .label(t(cx, if testing { "common.testing" } else { "common.test" }))
                        .disabled(testing || saving)
                        .on_click(cx.listener(|form, _, window, cx| form.test(window, cx))),
                )
                .child(
                    Button::new("connection-save")
                        .debug_selector(|| "connection-save".into())
                        .primary()
                        .label(t(cx, "common.save"))
                        .disabled(testing || saving)
                        .on_click(cx.listener(|form, _, window, cx| form.save(window, cx))),
                ),
        )
    }
}

impl ConnectionForm {
    fn ssh_section(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let f = &self.fields;
        let enabled = self.toggle(
            "conn-ssh",
            self.ssh_enabled,
            "connection.sshTunnel",
            "connection.sshTunnelHint",
            |form| form.ssh_enabled = !form.ssh_enabled,
            cx,
        );
        let mut section = form::section(cx).child(enabled);
        if !self.ssh_enabled {
            return section.into_any_element();
        }
        let auth = vec![
            ("key".into(), t(cx, "connection.sshAuthKey")),
            ("password".into(), t(cx, "connection.sshAuthPassword")),
            ("agent".into(), t(cx, "connection.sshAuthAgent")),
        ];
        let auth_select = self.select(
            "conn-ssh-auth",
            self.ssh_auth.clone().into(),
            auth,
            |form, value, _, cx| {
                form.ssh_auth = value.to_string();
                cx.notify();
            },
            cx,
        );
        section = section
            .child(form::row(
                form::group(t(cx, "connection.sshHost"), form::input(&f.ssh_host, window, cx), cx),
                form::group(t(cx, "connection.sshPort"), form::input(&f.ssh_port, window, cx), cx),
            ))
            .child(form::row(
                form::group(t(cx, "connection.sshUsername"), form::input(&f.ssh_username, window, cx), cx),
                form::group(t(cx, "connection.sshAuth"), auth_select, cx),
            ));
        section = match self.ssh_auth.as_str() {
            "password" => section.child(form::group(
                t(cx, "connection.sshPassword"),
                form::input(&f.ssh_password, window, cx).mask_toggle(),
                cx,
            )),
            "agent" => section.child(form::hint(t(cx, "connection.sshAuthAgentHint"), cx).mt_0()),
            _ => {
                let key_row = self.browse_row(&f.ssh_key_path, "browse-ssh-key", Browse::SshKey, window, cx);
                section.child(form::group(t(cx, "connection.sshKeyPath"), key_row, cx)).child(form::group(
                    t(cx, "connection.sshPassphrase"),
                    form::input(&f.ssh_passphrase, window, cx).mask_toggle(),
                    cx,
                ))
            }
        };
        section = section.child(self.toggle(
            "conn-ssh-ignore-host-key",
            self.ignore_host_key,
            "connection.sshIgnoreHostKey",
            "connection.sshIgnoreHostKeyHint",
            |form| form.ignore_host_key = !form.ignore_host_key,
            cx,
        ));
        if !self.ignore_host_key {
            let known_hosts_row =
                self.browse_row(&f.ssh_known_hosts, "browse-known-hosts", Browse::KnownHosts, window, cx);
            let hint = t_with(cx, "connection.sshKnownHostsHint", &[("path", &path_defaults().ssh_known_hosts)]);
            section = section
                .child(form::group(t(cx, "connection.sshKnownHosts"), known_hosts_row, cx).child(form::hint(hint, cx)));
        }
        section.into_any_element()
    }
}

// None or a config with an empty id is a new connection. Enter in a field doesn't close the dialog.
pub fn open(
    config: Option<ConnectionConfig>,
    window: &mut Window,
    cx: &mut App,
    on_saved: impl Fn(ConnectionConfig, &mut Window, &mut App) + 'static,
) -> Entity<ConnectionForm> {
    let config = config.unwrap_or_else(new_connection);
    let editing = !config.id.is_empty();
    let on_saved: OnSaved = Rc::new(on_saved);
    let form = cx.new(|cx| ConnectionForm::new(config, on_saved, window, cx));
    #[cfg(all(test, feature = "e2e"))]
    cx.set_global(Opened(form.downgrade()));
    let handle = form.clone();
    let name = form.read(cx).fields.name.clone();
    window.open_dialog(cx, move |dialog, window, cx| {
        let title = t(cx, if editing { "connection.editTitle" } else { "connection.newTitle" });
        Modal::new("connection", title)
            .size(modal::Size::Rem(WIDTH))
            .build(dialog, form.clone(), window, cx)
            .on_ok(|_, _, _| false)
    });
    window.defer(cx, move |window, cx| name.update(cx, |state, cx| state.focus(window, cx)));
    handle
}

#[cfg(test)]
mod tests {
    use barsql_core::{ConnectionConfig, DriverType};
    use gpui_kit::component::input::InputState;

    use super::{COLORS, ConnectionForm, DEFAULT_COLOR, Fields, new_connection, open, parse_port};

    #[test]
    fn ports_parse_leading_digits_with_a_fallback() {
        assert_eq!(parse_port("5433", 5432), 5433);
        assert_eq!(parse_port(" 3307abc", 3306), 3307);
        assert_eq!(parse_port("", 5432), 5432);
        assert_eq!(parse_port("0", 22), 22);
        assert_eq!(parse_port("x1", 22), 22);
    }

    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::component::Root;
    use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext};

    use crate::test_support::{Env, settle};

    struct Blank;

    impl gpui_kit::Render for Blank {
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut gpui_kit::Context<Self>) -> impl gpui_kit::IntoElement {
            use gpui_kit::Styled as _;
            gpui_kit::div().size_full()
        }
    }

    fn window(cx: &mut TestAppContext) -> &mut VisualTestContext {
        let window = cx.add_window(|window, cx| Root::new(cx.new(|_| Blank), window, cx));
        VisualTestContext::from_window(window.into(), cx).into_mut()
    }

    fn open_form(
        config: Option<ConnectionConfig>,
        cx: &mut VisualTestContext,
    ) -> (Entity<ConnectionForm>, Rc<RefCell<Vec<ConnectionConfig>>>) {
        let saved = Rc::new(RefCell::new(Vec::new()));
        let sink = saved.clone();
        let form = cx.update(|window, cx| open(config, window, cx, move |config, _, _| sink.borrow_mut().push(config)));
        (form, saved)
    }

    fn fill(
        form: &Entity<ConnectionForm>,
        field: fn(&Fields) -> &Entity<InputState>,
        value: &str,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| {
            let state = field(&form.read(cx).fields).clone();
            ConnectionForm::set(&state, value, window, cx);
        });
    }

    fn error(form: &Entity<ConnectionForm>, cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|_, cx| form.read(cx).error.as_ref().map(|e| e.to_string()))
    }

    #[gpui_kit::test]
    fn saving_validates_then_stores_the_trimmed_connection(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let cx = window(cx);
        let (form, saved) = open_form(None, cx);
        form.update_in(cx, |form, window, cx| form.save(window, cx));
        assert_eq!(error(&form, cx).as_deref(), Some("Connection name is required."));
        fill(&form, |f| &f.name, "Notes", cx);
        form.update_in(cx, |form, window, cx| form.save(window, cx));
        assert_eq!(error(&form, cx).as_deref(), Some("Database file is required."));

        form.update_in(cx, |form, window, cx| form.set_driver(DriverType::Postgres, window, cx));
        form.update_in(cx, |form, window, cx| form.save(window, cx));
        assert!(error(&form, cx).unwrap().starts_with("Database name is required"));
        fill(&form, |f| &f.database, "  forum  ", cx);
        fill(&form, |f| &f.host, " db.local ", cx);
        fill(&form, |f| &f.schema, "   ", cx);
        form.update_in(cx, |form, window, cx| form.save(window, cx));
        settle(cx, |_| !saved.borrow().is_empty());
        let stored = saved.borrow()[0].clone();
        assert_eq!((stored.database.as_str(), stored.host.as_str()), ("forum", "db.local"));
        assert_eq!((stored.schema.as_str(), stored.port, stored.username.as_str()), ("public", 5432, "postgres"));
        assert!(!stored.id.is_empty());
        assert!(env.bar.list_connections().iter().any(|c| c.id == stored.id && c.name == "Notes"));
    }

    #[gpui_kit::test]
    fn a_mysql_browse_database_defaults_to_the_connection_database(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let cx = window(cx);
        let (form, saved) = open_form(None, cx);
        form.update_in(cx, |form, window, cx| form.set_driver(DriverType::MySql, window, cx));
        let defaults = cx.update(|_, cx| {
            let f = &form.read(cx).fields;
            (
                ConnectionForm::value(&f.port, cx),
                ConnectionForm::value(&f.username, cx),
                ConnectionForm::value(&f.schema, cx),
            )
        });
        assert_eq!(defaults, ("3306".to_string(), "root".to_string(), String::new()));
        fill(&form, |f| &f.name, "Shop", cx);
        fill(&form, |f| &f.database, "shop", cx);
        form.update_in(cx, |form, window, cx| form.save(window, cx));
        settle(cx, |_| !saved.borrow().is_empty());
        assert_eq!(saved.borrow()[0].schema, "shop");
    }

    #[gpui_kit::test]
    fn editing_keeps_the_id_folder_and_unknown_fields(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let mut original = env.connection.clone();
        original.folder_id = "f1".into();
        original.extra.insert("futureField".into(), serde_json::json!(true));
        let cx = window(cx);
        let (form, saved) = open_form(Some(original.clone()), cx);
        fill(&form, |f| &f.name, "Renamed", cx);
        form.update_in(cx, |form, window, cx| form.save(window, cx));
        settle(cx, |_| !saved.borrow().is_empty());
        let stored = saved.borrow()[0].clone();
        assert_eq!((stored.id, stored.folder_id, stored.name), (original.id, "f1".to_string(), "Renamed".to_string()));
        assert_eq!(stored.extra.get("futureField"), Some(&serde_json::json!(true)));
    }

    // At 11px and 17px on the 2x test screen, device-pixel rounding pushed a row's last swatch out of a 480px dialog.
    #[gpui_kit::test]
    fn the_colours_fit_in_two_rows_at_any_zoom(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let cx = window(cx);
        for zoom in [10., 11., 13., 17.] {
            cx.update(|window, cx| crate::theme::set_zoom(zoom, window, cx));
            open_form(None, cx);
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let tops: Vec<_> = COLORS
                .iter()
                .map(|color| cx.debug_bounds(Box::leak(format!("swatch-{color}").into_boxed_str())).map(|b| b.top()))
                .collect();
            assert!(tops.iter().all(Option::is_some), "every swatch is drawn at {zoom}px");
            assert!(tops[..15].iter().all(|top| *top == tops[0]), "the first 15 share a row at {zoom}px");
            assert!(
                tops[15..].iter().all(|top| *top == tops[15] && *top > tops[0]),
                "the rest fill one more at {zoom}px"
            );
            cx.update(gpui_kit::component::WindowExt::close_dialog);
            cx.run_until_parked();
        }
    }

    #[gpui_kit::test]
    fn a_picked_database_fills_the_field(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let cx = window(cx);
        let (form, _) = open_form(None, cx);
        let fields = |form: &Entity<ConnectionForm>, cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                let f = &form.read(cx).fields;
                (ConnectionForm::value(&f.database, cx), ConnectionForm::value(&f.schema, cx))
            })
        };
        form.update_in(cx, |form, window, cx| form.set_driver(DriverType::MySql, window, cx));
        form.update_in(cx, |form, window, cx| form.pick_database("shop", window, cx));
        assert_eq!(fields(&form, cx), ("shop".to_string(), "shop".to_string()));
        fill(&form, |f| &f.schema, "reports", cx);
        form.update_in(cx, |form, window, cx| form.pick_database("blog", window, cx));
        assert_eq!(fields(&form, cx), ("blog".to_string(), "reports".to_string()));
        form.update_in(cx, |form, window, cx| form.set_driver(DriverType::Postgres, window, cx));
        form.update_in(cx, |form, window, cx| form.pick_database("forum", window, cx));
        assert_eq!(fields(&form, cx), ("forum".to_string(), "public".to_string()));
    }

    #[gpui_kit::test]
    fn the_database_list_is_kept_per_server(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let cx = window(cx);
        let (form, _) = open_form(None, cx);
        form.update_in(cx, |form, window, cx| form.set_driver(DriverType::Postgres, window, cx));
        let key = |cx: &mut VisualTestContext| cx.update(|_, cx| form.read(cx).server_key(cx));
        let first = key(cx);
        fill(&form, |f| &f.name, "Renamed", cx);
        fill(&form, |f| &f.database, "other", cx);
        assert_eq!(key(cx), first);
        fill(&form, |f| &f.host, "db.example.com", cx);
        assert_ne!(key(cx), first);
    }

    #[test]
    fn a_new_connection_starts_with_the_defaults() {
        let fresh = new_connection();
        assert_eq!((fresh.driver, fresh.color.as_str(), fresh.port), (DriverType::Sqlite, DEFAULT_COLOR, 5432));
        assert_eq!(
            (fresh.host.as_str(), fresh.username.as_str(), fresh.schema.as_str()),
            ("localhost", "postgres", "public")
        );
        assert_eq!(COLORS.len(), 30);
    }
}
