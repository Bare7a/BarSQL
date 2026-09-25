// Whole-window tests. `Driver` sends keys, clicks and menu actions to the window, on Env's SQLite database.
mod driver;

mod connections;
mod editor;
#[cfg(feature = "e2e")]
mod engines;
mod import;
mod plans;
mod results;
mod schema;
mod shell;
mod sidebar;
mod table_view;
