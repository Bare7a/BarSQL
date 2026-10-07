use barsql_core::{ConnectionConfig, DriverType, Value};

use super::session::inline_nulls;
use super::{MsOptions, MsTls, line_position};

fn config(host: &str, ssl_mode: &str) -> ConnectionConfig {
    ConnectionConfig {
        driver: DriverType::SqlServer,
        host: host.into(),
        username: "sa".into(),
        ssl_mode: ssl_mode.into(),
        ..Default::default()
    }
}

#[test]
fn options_take_a_named_instance_from_either_place() {
    let named = MsOptions::from_config(&config(r"db.example.com\SQLEXPRESS", "")).unwrap();
    assert_eq!((named.host.as_str(), named.instance.as_deref()), ("db.example.com", Some("SQLEXPRESS")));
    assert_eq!((named.port, named.tls, named.schema.as_str()), (1433, MsTls::Require, "dbo"));
    let field = MsOptions::from_config(&ConnectionConfig { instance: "DEV".into(), ..config("db", "strict") }).unwrap();
    assert_eq!((field.instance.as_deref(), field.tls), (Some("DEV"), MsTls::Strict));
    assert!(MsOptions::from_config(&config("db", "maybe")).is_err());

    let mut tunneled = config(r"db\SQLEXPRESS", "disable");
    tunneled.ssh.enabled = true;
    tunneled.ssh.host = "bastion".into();
    tunneled.ssh.username = "me".into();
    tunneled.ssh.password = "pw".into();
    tunneled.ssh.auth = "password".into();
    let error = MsOptions::from_config(&tunneled).unwrap_err();
    assert!(error.message.contains("named instance"), "{error:?}");
}

#[test]
fn an_errors_line_becomes_where_that_line_starts() {
    let sql = "SELECT 1;\n  SELECT nope;\nSELECT 3";
    assert_eq!(line_position(sql, 1), 1);
    assert_eq!(line_position(sql, 2), 13, "after the newline and the indent");
    assert_eq!(line_position(sql, 3), 26);
    assert_eq!(line_position(sql, 9), 0);
    assert_eq!(line_position(sql, 0), 0);
    assert_eq!(line_position("é;\nx", 2), 4, "counted in characters");
}

#[test]
fn null_parameters_are_written_in_and_the_rest_renumbered() {
    let sql = "UPDATE [t] SET [a] = @P1, [b] = @P2, [c] = '@P3' WHERE [id] = @P3 -- @P2";
    let params = [Value::Text("x".into()), Value::Null, Value::Int(7)];
    let (sql, kept) = inline_nulls(sql, &params);
    assert_eq!(sql, "UPDATE [t] SET [a] = @P1, [b] = NULL, [c] = '@P3' WHERE [id] = @P2 -- @P2");
    assert_eq!(kept, [Value::Text("x".into()), Value::Int(7)]);
    let untouched = inline_nulls("SELECT @P1", &[Value::Int(1)]);
    assert_eq!(untouched, ("SELECT @P1".to_string(), vec![Value::Int(1)]));
}

// A login is a future of tens of kilobytes. Inline, it rode along in every catalog call, and debug builds gave each
// caller a stack slot for it, until deep calls ran out of stack.
#[test]
fn catalog_futures_leave_the_login_boxed() {
    use std::sync::Arc;
    let options = MsOptions::from_config(&config("db", "")).unwrap();
    let engine = Arc::new(super::MsEngine {
        options,
        tunnel: None,
        idle: std::sync::Mutex::new(Vec::new()),
        permits: Arc::new(tokio::sync::Semaphore::new(1)),
    });
    let object =
        barsql_core::ObjectRef { name: "t".into(), kind: barsql_core::ObjectKind::Table, ..Default::default() };
    assert!(std::mem::size_of_val(&engine.lease()) < 4096);
    assert!(std::mem::size_of_val(&engine.rows("SELECT 1", &[])) < 16 * 1024);
    assert!(std::mem::size_of_val(&engine.object_ddl(&object)) < 16 * 1024);
}
