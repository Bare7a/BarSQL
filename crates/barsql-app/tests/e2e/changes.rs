use barsql_app::{BackupOutcome, BackupRequest, SqlImportRequest};
use barsql_core::{ObjectKind, ObjectRef};
use barsql_sql::alter::{self, ConstraintKind};

use crate::harness::{E2e, unique_table};
use crate::support::{import_outcome, text};

async fn snapshot(e: &E2e, sql: &str) -> Vec<Vec<String>> {
    e.query(sql).await.rows.iter().map(|row| row.iter().map(text).collect()).collect()
}

async fn change(e: &E2e, sql: &str) {
    if let Err(err) = e.app.execute_statement(&e.id, "e2e-change", sql).await {
        panic!("[{}] {sql}: {err:?}", e.kind.name());
    }
}

async fn tables(e: &E2e) -> Vec<String> {
    e.app.list_tables(&e.id, &e.schema()).await.unwrap().into_iter().map(|t| t.name).collect()
}

async fn columns(e: &E2e, table: &str) -> Vec<String> {
    e.app.list_columns(&e.id, &e.schema(), table).await.unwrap().into_iter().map(|c| c.name).collect()
}

async fn restore(e: &E2e, path: &str) {
    let req = SqlImportRequest { path: path.into(), stop_on_error: true };
    let (_, done) = import_outcome(e.app.import_sql(&e.id, "e2e-restore", req).await.unwrap()).await;
    assert!(done.result.is_some(), "[{}] restore: {}", e.kind.name(), done.error);
}

// A missing database falls back to the server's list, but a bad password is an error.
each_engine!(async fn the_database_list_names_the_test_database(e) {
    let mut cfg = e.kind.config();
    let listed = e.app.list_databases(cfg.clone()).await.unwrap();
    assert!(listed.contains(&cfg.database), "{listed:?}");
    assert!(!listed.iter().any(|d| d == "information_schema" || d.starts_with("template")), "{listed:?}");
    cfg.database = String::new();
    assert_eq!(e.app.list_databases(cfg.clone()).await.unwrap(), listed, "no database named yet");
    cfg.database = "barsql_no_such_database".into();
    assert_eq!(e.app.list_databases(cfg.clone()).await.unwrap(), listed, "a missing database falls back");
    cfg.schema = "barsql_no_such_schema".into();
    assert_eq!(e.app.list_databases(cfg.clone()).await.unwrap(), listed, "MySQL's browse schema is left out");
    cfg.password = "not-the-password".into();
    assert!(e.app.list_databases(cfg).await.is_err());
});

// Restores through the SQL import. The next generated id has to follow the restored rows.
each_engine!(async fn a_backup_restores_the_table_exactly(e) {
    let table = unique_table("backup");
    let q = e.qualified(&table);
    let (ddl, insert, select) = if e.is_postgres() {
        (
            format!(
                "CREATE TABLE {q} (
                    id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                    name TEXT NOT NULL,
                    price NUMERIC(10, 2),
                    ratio DOUBLE PRECISION,
                    active BOOLEAN,
                    raw BYTEA,
                    at TIMESTAMPTZ,
                    day DATE,
                    doc JSONB,
                    tags TEXT[],
                    shout TEXT GENERATED ALWAYS AS (upper(name)) STORED
                )"
            ),
            format!(
                "INSERT INTO {q} (name, price, ratio, active, raw, at, day, doc, tags) VALUES
                    ('it''s a \\ backslash', 12.50, 0.1, true, '\\xdeadbeef', '2024-01-02 03:04:05.123456+00',
                        '2024-02-29', '{{\"a\": [1, \"x\"]}}', '{{a,\"b c\"}}'),
                    (E'line\\nbreak ünï', NULL, 1e-300, false, '', NULL, NULL, 'null', '{{}}'),
                    ('third', 0, 'NaN', NULL, NULL, 'infinity', 'infinity', NULL, NULL)"
            ),
            format!(
                "SELECT id, name, price::text, ratio::text, active::text, encode(raw, 'hex'), at::text, day::text,
                    doc::text, tags::text, shout FROM {q} ORDER BY id"
            ),
        )
    } else {
        (
            format!(
                "CREATE TABLE {q} (
                    id INT AUTO_INCREMENT PRIMARY KEY,
                    name VARCHAR(100) NOT NULL,
                    price DECIMAL(10, 2),
                    ratio DOUBLE,
                    active TINYINT(1),
                    raw VARBINARY(16),
                    at DATETIME(6),
                    day DATE,
                    doc JSON,
                    flags BIT(8),
                    mood ENUM('ok', 'meh'),
                    weight FLOAT,
                    seen TIMESTAMP(3) NULL,
                    shout VARCHAR(100) AS (UPPER(name)) STORED
                )"
            ),
            format!(
                "INSERT INTO {q} (name, price, ratio, active, raw, at, day, doc, flags, mood, weight, seen) VALUES
                    ('it''s a \\\\ backslash', 12.50, 0.1, 1, X'DEADBEEF', '2024-01-02 03:04:05.123456',
                        '2024-02-29', '{{\"a\": [1, \"x\"]}}', b'10100101', 'ok', 1234567, '2024-03-31 01:30:00.250'),
                    ('line\\nbreak ünï', NULL, 1e-300, 0, X'', NULL, NULL, 'null', b'0', 'meh', 12345.67, NULL),
                    ('third', 0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)"
            ),
            format!(
                "SELECT id, name, CAST(price AS CHAR), CAST(ratio AS CHAR), active, HEX(raw), CAST(at AS CHAR),
                    CAST(day AS CHAR), CAST(doc AS CHAR), CAST(flags + 0 AS CHAR), mood, shout,
                    CAST(weight + 0E0 AS CHAR), UNIX_TIMESTAMP(seen) FROM {q} ORDER BY id"
            ),
        )
    };
    e.create_temp_table(&ddl, &table).await;
    e.exec(&insert).await;
    let before = snapshot(&e, &select).await;
    assert_eq!(before.len(), 3);

    let path = e.write(&format!("{table}.sql"), "");
    let req = BackupRequest { schema: e.schema(), table: table.clone(), structure: true, data: true, path: path.clone().into() };
    let (tx, _rx) = async_channel::unbounded();
    let outcome = e.app.backup_table(&e.id, "e2e-backup", req, tx).await.unwrap();
    assert_eq!(outcome, BackupOutcome { rows: 3, cancelled: false });

    e.exec(&format!("DROP TABLE {q}")).await;
    restore(&e, &path).await;
    assert_eq!(snapshot(&e, &select).await, before, "{}", std::fs::read_to_string(&path).unwrap());
    e.exec(&format!("INSERT INTO {q} (name) VALUES ('next')")).await;
    assert_eq!(snapshot(&e, &format!("SELECT MAX(id) FROM {q}")).await, [["4"]]);
});

each_engine!(async fn table_and_column_changes_run(e) {
    let (driver, schema) = (e.driver(), e.schema());
    let table = unique_table("change");
    let renamed = format!("{table}_r");
    let (view, view_renamed) = (unique_table("change_v"), unique_table("change_w"));
    e.create_temp_table(&e.auto_pk_table(&e.qualified(&table)), &table).await;
    e.defer(format!("DROP TABLE IF EXISTS {}", e.qualified(&renamed)));
    e.exec(&format!("ALTER TABLE {} ADD COLUMN extra INT", e.qualified(&table))).await;
    e.exec(&format!("INSERT INTO {} (name) VALUES ('a'), ('b')", e.qualified(&table))).await;

    change(&e, &alter::rename_column(&driver, &schema, &table, "name", "label")).await;
    change(&e, &alter::drop_column(&driver, &schema, &table, "extra", false)).await;
    assert_eq!(columns(&e, &table).await, ["id", "label"]);

    change(&e, &alter::truncate_table(&driver, &schema, &table, true, false)).await;
    assert_eq!(e.app.count_rows(&e.id, &schema, &table).await.unwrap(), 0);
    e.exec(&format!("INSERT INTO {} (label) VALUES ('c')", e.qualified(&table))).await;
    assert_eq!(snapshot(&e, &format!("SELECT id FROM {}", e.qualified(&table))).await, [["1"]], "the ids restart");

    change(&e, &alter::rename_relation(&driver, &ObjectKind::Table, &schema, &table, &renamed).unwrap()).await;
    let listed = tables(&e).await;
    assert!(listed.contains(&renamed) && !listed.contains(&table), "{listed:?}");

    e.exec(&format!("CREATE VIEW {} AS SELECT id FROM {}", e.qualified(&view), e.qualified(&renamed))).await;
    e.defer(format!("DROP VIEW IF EXISTS {}", e.qualified(&view)));
    e.defer(format!("DROP VIEW IF EXISTS {}", e.qualified(&view_renamed)));
    change(&e, &alter::rename_relation(&driver, &ObjectKind::View, &schema, &view, &view_renamed).unwrap()).await;
    assert!(tables(&e).await.contains(&view_renamed));
    change(&e, &alter::drop_relation(&driver, &ObjectKind::View, &schema, &view_renamed, false)).await;
    change(&e, &alter::drop_relation(&driver, &ObjectKind::Table, &schema, &renamed, false)).await;
    let listed = tables(&e).await;
    assert!(!listed.contains(&renamed) && !listed.contains(&view_renamed), "{listed:?}");
});

// Postgres refuses to drop a table a view depends on unless the drop cascades.
each_engine!(async fn a_cascading_drop_takes_the_dependents(e) {
    if !e.is_postgres() {
        return;
    }
    let (driver, schema) = (e.driver(), e.schema());
    let (table, view) = (unique_table("cascade"), unique_table("cascade_v"));
    e.create_temp_table(&e.auto_pk_table(&e.qualified(&table)), &table).await;
    e.exec(&format!("CREATE VIEW {} AS SELECT id FROM {}", e.qualified(&view), e.qualified(&table))).await;
    e.defer(format!("DROP VIEW IF EXISTS {}", e.qualified(&view)));
    let plain = alter::drop_relation(&driver, &ObjectKind::Table, &schema, &table, false);
    let err = e.app.execute_statement(&e.id, "e2e-change", &plain).await.unwrap_err();
    assert_eq!(err.code, "2BP01", "{err:?}");
    change(&e, &alter::drop_relation(&driver, &ObjectKind::Table, &schema, &table, true)).await;
    let listed = tables(&e).await;
    assert!(!listed.contains(&table) && !listed.contains(&view), "{listed:?}");
});

each_engine!(async fn object_drops_run(e) {
    let (driver, schema) = (e.driver(), e.schema());
    let (parent, child) = (unique_table("objp"), unique_table("objc"));
    let (pq, cq) = (e.qualified(&parent), e.qualified(&child));
    e.create_temp_table(&e.auto_pk_table(&pq), &parent).await;
    e.create_temp_table(
        &format!(
            "CREATE TABLE {cq} (id INT PRIMARY KEY, email VARCHAR(100), parent_id INT, qty INT,
                CONSTRAINT {child}_uq UNIQUE (email),
                CONSTRAINT {child}_ck CHECK (qty > 0),
                CONSTRAINT {child}_fk FOREIGN KEY (parent_id) REFERENCES {pq} (id))"
        ),
        &child,
    )
    .await;
    e.exec(&format!("CREATE INDEX {child}_idx ON {cq} (qty)")).await;
    let (touch, add, tidy) = (format!("{child}_touch"), format!("{child}_add"), format!("{child}_tidy"));
    if e.is_postgres() {
        e.exec(&format!("CREATE FUNCTION {touch}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$")).await;
        e.defer(format!("DROP FUNCTION IF EXISTS {touch}()"));
        e.exec(&format!("CREATE TRIGGER {child}_trg BEFORE INSERT ON {cq} FOR EACH ROW EXECUTE FUNCTION {touch}()")).await;
        e.exec(&format!("CREATE FUNCTION {add}(a integer, b integer) RETURNS integer LANGUAGE sql AS 'SELECT a + b'")).await;
        e.exec(&format!("CREATE PROCEDURE {tidy}() LANGUAGE sql AS 'SELECT 1'")).await;
    } else {
        e.exec(&format!("CREATE TRIGGER {child}_trg BEFORE INSERT ON {cq} FOR EACH ROW SET NEW.qty = NEW.qty")).await;
        e.exec(&format!("CREATE FUNCTION {add}(a INT, b INT) RETURNS INT DETERMINISTIC RETURN a + b")).await;
        e.exec(&format!("CREATE PROCEDURE {tidy}() SELECT 1")).await;
    }
    e.defer(format!("DROP FUNCTION IF EXISTS {add}{}", if e.is_postgres() { "(integer, integer)" } else { "" }));
    e.defer(format!("DROP PROCEDURE IF EXISTS {tidy}{}", if e.is_postgres() { "()" } else { "" }));
    let drop = |object: &ObjectRef, kind: ConstraintKind| {
        alter::drop_object(&driver, object, kind, false).unwrap_or_else(|| panic!("no DROP for {object:?}"))
    };

    let index = e.app.list_indexes(&e.id, &schema, &child).await.unwrap().into_iter().find(|i| i.name == format!("{child}_idx"));
    let index = index.expect("the index is listed");
    let object = ObjectRef { schema: index.schema, name: index.name, kind: ObjectKind::Index, table: index.table, args: String::new() };
    change(&e, &drop(&object, ConstraintKind::Other)).await;
    assert!(!e.app.list_indexes(&e.id, &schema, &child).await.unwrap().iter().any(|i| i.name == object.name));

    let trigger = e.app.list_triggers(&e.id, &schema, &child).await.unwrap().remove(0);
    let object = ObjectRef { schema: trigger.schema, name: trigger.name, kind: ObjectKind::Trigger, table: trigger.table, args: String::new() };
    change(&e, &drop(&object, ConstraintKind::Other)).await;
    assert!(e.app.list_triggers(&e.id, &schema, &child).await.unwrap().is_empty());

    // Drop the foreign key first since MySQL's uses an index the other constraints may share.
    for kind in ["FOREIGN KEY", "UNIQUE", "CHECK", "PRIMARY KEY"] {
        let constraints = e.app.list_constraints(&e.id, &schema, &child).await.unwrap();
        let found = constraints.into_iter().find(|c| c.kind == kind).unwrap_or_else(|| panic!("no {kind} listed"));
        let object = ObjectRef { schema: found.schema, name: found.name, kind: ObjectKind::Constraint, table: found.table, args: String::new() };
        change(&e, &drop(&object, ConstraintKind::parse(kind))).await;
        let left = e.app.list_constraints(&e.id, &schema, &child).await.unwrap();
        assert!(!left.iter().any(|c| c.kind == kind), "[{}] {kind} still listed: {left:?}", e.kind.name());
    }

    for name in [&add, &tidy] {
        let routine = e.app.list_routines(&e.id, &schema).await.unwrap().into_iter().find(|r| &r.name == name);
        let routine = routine.unwrap_or_else(|| panic!("{name} is listed"));
        let object = ObjectRef { schema: routine.schema, name: routine.name, kind: routine.kind, table: String::new(), args: routine.args };
        change(&e, &drop(&object, ConstraintKind::Other)).await;
        assert!(!e.app.list_routines(&e.id, &schema).await.unwrap().iter().any(|r| &r.name == name));
    }
});

each_engine!(async fn a_backup_reads_the_schema_it_was_asked_for(e) {
    let other = unique_table("backup_schema");
    let table = unique_table("twin");
    let create = if e.is_postgres() { format!("CREATE SCHEMA {other}") } else { format!("CREATE DATABASE {other}") };
    e.exec(&create).await;
    e.defer(if e.is_postgres() { format!("DROP SCHEMA IF EXISTS {other} CASCADE") } else { format!("DROP DATABASE IF EXISTS {other}") });
    let (here, there) = (e.qualified(&table), barsql_sql::qualified_table(&e.driver(), &other, &table));
    e.create_temp_table(&format!("CREATE TABLE {here} (v VARCHAR(20))"), &table).await;
    e.exec(&format!("INSERT INTO {here} VALUES ('default')")).await;
    e.exec(&format!("CREATE TABLE {there} (v VARCHAR(20))")).await;
    e.exec(&format!("INSERT INTO {there} VALUES ('other')")).await;
    let path = e.write(&format!("{table}.sql"), "");
    let req = BackupRequest { schema: other.clone(), table: table.clone(), structure: false, data: true, path: path.clone().into() };
    let (tx, _rx) = async_channel::unbounded();
    e.app.backup_table(&e.id, "e2e-backup", req, tx).await.unwrap();
    let script = std::fs::read_to_string(&path).unwrap();
    assert!(script.contains("'other'") && !script.contains("'default'"), "{script}");
});

// Postgres adds its foreign keys after the data, and MySQL turns the checks off while the file runs.
each_engine!(async fn a_self_referencing_table_restores(e) {
    let table = unique_table("staff");
    let q = e.qualified(&table);
    e.create_temp_table(&format!("CREATE TABLE {q} (id INT PRIMARY KEY, manager_id INT, FOREIGN KEY (manager_id) REFERENCES {q} (id))"), &table).await;
    // InnoDB checks each row, so insert the manager first. The backup still writes it last, since MySQL
    // reads rows by id and the UPDATE moves it to the end of the Postgres heap.
    e.exec(&format!("INSERT INTO {q} VALUES (250, NULL)")).await;
    let rows: Vec<String> = (1..250).map(|i| format!("({i}, 250)")).collect();
    e.exec(&format!("INSERT INTO {q} VALUES {}", rows.join(", "))).await;
    e.exec(&format!("UPDATE {q} SET manager_id = NULL WHERE id = 250")).await;
    let path = e.write(&format!("{table}.sql"), "");
    let req = BackupRequest { schema: e.schema(), table: table.clone(), structure: true, data: true, path: path.clone().into() };
    let (tx, _rx) = async_channel::unbounded();
    e.app.backup_table(&e.id, "e2e-backup", req, tx).await.unwrap();
    e.exec(&format!("DROP TABLE {q}")).await;
    restore(&e, &path).await;
    assert_eq!(e.count(&table).await, 250);
    let constraints = e.app.list_constraints(&e.id, &e.schema(), &table).await.unwrap();
    assert!(constraints.iter().any(|c| c.kind == "FOREIGN KEY"), "the key is back: {constraints:?}");
});

// MySQL can't quote a value over max_allowed_packet, so the backup must fail rather than write NULL.
each_engine!(async fn an_oversized_value_fails_the_backup(e) {
    if e.is_postgres() {
        return;
    }
    let packet = snapshot(&e, "SELECT @@max_allowed_packet").await[0][0].parse::<u64>().unwrap();
    let table = unique_table("huge");
    let q = e.qualified(&table);
    e.create_temp_table(&format!("CREATE TABLE {q} (b LONGBLOB)"), &table).await;
    e.exec(&format!("INSERT INTO {q} VALUES (REPEAT('x', {}))", packet / 2 + 1)).await;
    let path = e.write(&format!("{table}.sql"), "untouched");
    let req = BackupRequest { schema: e.schema(), table, structure: false, data: true, path: path.clone().into() };
    let (tx, _rx) = async_channel::unbounded();
    let err = e.app.backup_table(&e.id, "e2e-backup", req, tx).await.unwrap_err();
    assert!(err.message.contains("max_allowed_packet"), "{err:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "untouched");
});
