use barsql_core::{ConstraintInfo, DriverType, IndexInfo, ObjectKind, SqlDialect};

use crate::dialect::Dialect;
use crate::quote::{qualified_table, quote_ident, quote_ident_list};
use crate::sql_text::to_upper;

const INDENT: &str = "    ";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DdlColumn {
    pub name: String,
    pub data_type: String,
    pub not_null: bool,
    pub default: String,
    pub collation: String,
    // "ALWAYS" or "BY DEFAULT" on an identity column, or SQL Server's "seed, increment".
    pub identity: String,
    // Overrides `default`, since a generated column can't have one. SQL Server's includes PERSISTED.
    pub generated: String,
}

pub fn render_column(driver: &DriverType, col: &DdlColumn) -> String {
    if driver.dialect() == Some(SqlDialect::TSql) {
        return render_tsql_column(driver, col);
    }
    let mut out = quote_ident(driver, &col.name);
    if !col.data_type.is_empty() {
        out.push(' ');
        out.push_str(&col.data_type);
    }
    if !col.collation.is_empty() {
        out.push_str(" COLLATE ");
        out.push_str(&quote_ident(driver, &col.collation));
    }
    if !col.generated.is_empty() {
        out.push_str(&format!(" GENERATED ALWAYS AS ({}) STORED", col.generated));
    } else if !col.identity.is_empty() {
        out.push_str(&format!(" GENERATED {} AS IDENTITY", col.identity));
    } else if !col.default.is_empty() {
        out.push_str(" DEFAULT ");
        out.push_str(&col.default);
    }
    if col.not_null {
        out.push_str(" NOT NULL");
    }
    out
}

// A computed column has no type of its own, and a collation is named bare.
fn render_tsql_column(driver: &DriverType, col: &DdlColumn) -> String {
    let mut out = quote_ident(driver, &col.name);
    if !col.generated.is_empty() {
        out.push_str(&format!(" AS {}", col.generated));
        return out;
    }
    out.push(' ');
    out.push_str(&col.data_type);
    if !col.collation.is_empty() {
        out.push_str(&format!(" COLLATE {}", col.collation));
    }
    if !col.identity.is_empty() {
        out.push_str(&format!(" IDENTITY({})", col.identity));
    } else if !col.default.is_empty() {
        out.push_str(&format!(" DEFAULT {}", col.default));
    }
    if col.not_null {
        out.push_str(" NOT NULL");
    }
    out
}

pub fn compose_create_table(
    driver: &DriverType,
    schema: &str,
    table: &str,
    cols: &[DdlColumn],
    constraints: &[String],
) -> String {
    let lines: Vec<String> = cols
        .iter()
        .map(|col| format!("{INDENT}{}", render_column(driver, col)))
        .chain(constraints.iter().map(|c| format!("{INDENT}{c}")))
        .collect();
    let head = format!("CREATE TABLE {}", qualified_table(driver, schema, table));
    let suffix = Dialect::for_driver(driver).create_table_suffix;
    if lines.is_empty() {
        return format!("{head} (){suffix};");
    }
    format!("{head} (\n{}\n){suffix};", lines.join(",\n"))
}

// Prefers the engine's definition over a synthesized one. Empty when neither is usable.
pub fn render_constraint(driver: &DriverType, c: &ConstraintInfo) -> String {
    let body = if c.definition.is_empty() { synthesize_constraint_body(driver, c) } else { c.definition.clone() };
    if body.is_empty() {
        return String::new();
    }
    if c.name.is_empty() {
        return body;
    }
    format!("CONSTRAINT {} {body}", quote_ident(driver, &c.name))
}

fn synthesize_constraint_body(driver: &DriverType, c: &ConstraintInfo) -> String {
    let cols = quote_ident_list(driver, &c.columns);
    match to_upper(&c.kind).as_str() {
        "PRIMARY KEY" if !cols.is_empty() => format!("PRIMARY KEY ({cols})"),
        "UNIQUE" if !cols.is_empty() => format!("UNIQUE ({cols})"),
        "FOREIGN KEY" if !cols.is_empty() && !c.ref_table.is_empty() => {
            let mut target = quote_ident(driver, &c.ref_table);
            if !c.ref_columns.is_empty() {
                target.push_str(&format!(" ({})", quote_ident_list(driver, &c.ref_columns)));
            }
            format!("FOREIGN KEY ({cols}) REFERENCES {target}")
        }
        _ => String::new(),
    }
}

// Empty for a primary-key index, which has no standalone form.
pub fn render_create_index(driver: &DriverType, idx: &IndexInfo) -> String {
    if idx.is_primary || idx.columns.is_empty() {
        return String::new();
    }
    let unique = if idx.is_unique { "UNIQUE " } else { "" };
    // USING is Postgres-only syntax, though MySQL reports a method too.
    let using = if !idx.method.is_empty() && Dialect::for_driver(driver).index_using {
        format!(" USING {}", idx.method)
    } else {
        String::new()
    };
    format!(
        "CREATE {unique}INDEX {} ON {}{using} ({});",
        quote_ident(driver, &idx.name),
        qualified_table(driver, &idx.schema, &idx.table),
        quote_ident_list(driver, &idx.columns)
    )
}

pub fn join_ddl<S: AsRef<str>>(blocks: &[S]) -> String {
    blocks
        .iter()
        .map(|b| b.as_ref().trim_end_matches([' ', '\t', '\n']))
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn terminate_statement(sql: &str) -> String {
    let trimmed = sql.trim_end_matches([' ', '\t', '\r', '\n']);
    if trimmed.is_empty() || trimmed.ends_with(';') { trimmed.to_string() } else { format!("{trimmed};") }
}

pub fn unsupported_ddl(driver: &DriverType, kind: &ObjectKind) -> String {
    format!("{driver} does not expose DDL for {kind} objects")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, data_type: &str) -> DdlColumn {
        DdlColumn { name: name.into(), data_type: data_type.into(), ..Default::default() }
    }

    #[test]
    fn renders_columns() {
        let pg = DriverType::Postgres;
        let cases = [
            (col("email", "text"), r#""email" text"#),
            (
                DdlColumn { not_null: true, default: "now()".into(), ..col("created_at", "timestamptz") },
                r#""created_at" timestamptz DEFAULT now() NOT NULL"#,
            ),
            (
                DdlColumn {
                    not_null: true,
                    identity: "BY DEFAULT".into(),
                    default: "ignored".into(),
                    ..col("id", "bigint")
                },
                r#""id" bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL"#,
            ),
            (
                DdlColumn { generated: "qty * price".into(), default: "ignored".into(), ..col("total", "numeric") },
                r#""total" numeric GENERATED ALWAYS AS (qty * price) STORED"#,
            ),
            (DdlColumn { collation: "C".into(), ..col("name", "text") }, r#""name" text COLLATE "C""#),
            (col("we\"ird", "text"), r#""we""ird" text"#),
        ];
        for (col, want) in cases {
            assert_eq!(render_column(&pg, &col), want);
        }
    }

    #[test]
    fn sql_server_columns_use_its_own_syntax() {
        let ms = DriverType::SqlServer;
        let id = DdlColumn { identity: "1, 1".into(), not_null: true, ..col("id", "int") };
        assert_eq!(render_column(&ms, &id), "[id] int IDENTITY(1, 1) NOT NULL");
        let doubled = DdlColumn { generated: "([price]*(2)) PERSISTED".into(), ..col("doubled", "decimal(12, 2)") };
        assert_eq!(render_column(&ms, &doubled), "[doubled] AS ([price]*(2)) PERSISTED");
        let name = DdlColumn {
            default: "(N'anon')".into(),
            collation: "Latin1_General_BIN2".into(),
            not_null: true,
            ..col("name", "nvarchar(50)")
        };
        assert_eq!(
            render_column(&ms, &name),
            "[name] nvarchar(50) COLLATE Latin1_General_BIN2 DEFAULT (N'anon') NOT NULL"
        );
    }

    #[test]
    fn composes_create_table() {
        let got = compose_create_table(
            &DriverType::Postgres,
            "public",
            "users",
            &[
                DdlColumn { not_null: true, identity: "BY DEFAULT".into(), ..col("id", "bigint") },
                DdlColumn { not_null: true, ..col("email", "text") },
            ],
            &[r#"CONSTRAINT "users_pkey" PRIMARY KEY ("id")"#.into()],
        );
        let want = "CREATE TABLE \"public\".\"users\" (\n    \"id\" bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n    \"email\" text NOT NULL,\n    CONSTRAINT \"users_pkey\" PRIMARY KEY (\"id\")\n);";
        assert_eq!(got, want);

        let mysql = compose_create_table(
            &DriverType::MySql,
            "shop",
            "orders",
            &[DdlColumn { not_null: true, ..col("id", "int") }],
            &[],
        );
        assert!(mysql.contains("`shop`.`orders`") && mysql.contains("`id` int NOT NULL"), "{mysql}");
        assert_eq!(
            compose_create_table(&DriverType::Postgres, "public", "empty", &[], &[]),
            r#"CREATE TABLE "public"."empty" ();"#
        );
    }

    #[test]
    fn renders_constraints() {
        let c = |name: &str, kind: &str, cols: &[&str], def: &str| ConstraintInfo {
            name: name.into(),
            kind: kind.into(),
            columns: cols.iter().map(|s| s.to_string()).collect(),
            definition: def.into(),
            ..Default::default()
        };
        let pg = DriverType::Postgres;
        let fk = ConstraintInfo {
            ref_table: "orgs".into(),
            ref_columns: vec!["id".into()],
            ..c("fk_org", "FOREIGN KEY", &["org_id"], "")
        };
        let cases = [
            (c("ck_age", "CHECK", &[], "CHECK ((age > 0))"), r#"CONSTRAINT "ck_age" CHECK ((age > 0))"#),
            (c("users_pkey", "PRIMARY KEY", &["id"], ""), r#"CONSTRAINT "users_pkey" PRIMARY KEY ("id")"#),
            (c("u_ab", "UNIQUE", &["a", "b"], ""), r#"CONSTRAINT "u_ab" UNIQUE ("a", "b")"#),
            (fk, r#"CONSTRAINT "fk_org" FOREIGN KEY ("org_id") REFERENCES "orgs" ("id")"#),
            (c("", "PRIMARY KEY", &["id"], ""), r#"PRIMARY KEY ("id")"#),
            (c("ck", "CHECK", &[], ""), ""),
            (c("pk", "PRIMARY KEY", &[], ""), ""),
        ];
        for (constraint, want) in cases {
            assert_eq!(render_constraint(&pg, &constraint), want);
        }
    }

    #[test]
    fn renders_indexes() {
        let idx = |name: &str, schema: &str, table: &str, cols: &[&str]| IndexInfo {
            name: name.into(),
            schema: schema.into(),
            table: table.into(),
            columns: cols.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        assert_eq!(
            render_create_index(
                &DriverType::Postgres,
                &IndexInfo {
                    is_unique: true,
                    method: "btree".into(),
                    ..idx("users_email_idx", "public", "users", &["email"])
                }
            ),
            r#"CREATE UNIQUE INDEX "users_email_idx" ON "public"."users" USING btree ("email");"#
        );
        assert_eq!(
            render_create_index(
                &DriverType::MySql,
                &IndexInfo { method: "btree".into(), ..idx("idx_a", "shop", "orders", &["a", "b"]) }
            ),
            "CREATE INDEX `idx_a` ON `shop`.`orders` (`a`, `b`);"
        );
        assert_eq!(
            render_create_index(
                &DriverType::Postgres,
                &IndexInfo { is_primary: true, ..idx("users_pkey", "", "users", &["id"]) }
            ),
            ""
        );
        assert_eq!(render_create_index(&DriverType::Postgres, &idx("idx_expr", "", "users", &[])), "");
    }

    #[test]
    fn joins_and_terminates() {
        assert_eq!(
            join_ddl(&["CREATE TABLE a ();", "", "  \n", "CREATE INDEX i ON a (x);"]),
            "CREATE TABLE a ();\n\nCREATE INDEX i ON a (x);"
        );
        assert_eq!(join_ddl(&["", ""]), "");
        let cases = [
            ("CREATE TABLE t (a int)", "CREATE TABLE t (a int);"),
            ("CREATE TABLE t (a int);", "CREATE TABLE t (a int);"),
            ("CREATE TABLE t (a int);\n\n", "CREATE TABLE t (a int);"),
            ("", ""),
            ("   \n ", ""),
        ];
        for (sql, want) in cases {
            assert_eq!(terminate_statement(sql), want);
        }
    }

    #[test]
    fn relation_kinds() {
        let cases = [
            ("table", ObjectKind::Table),
            ("view", ObjectKind::View),
            ("VIEW", ObjectKind::View),
            ("materialized view", ObjectKind::MaterializedView),
            ("partitioned table", ObjectKind::Table),
            ("", ObjectKind::Table),
        ];
        for (table_type, want) in cases {
            assert_eq!(ObjectKind::for_relation(table_type), want);
        }
        assert!(
            [ObjectKind::Table, ObjectKind::View, ObjectKind::MaterializedView].iter().all(ObjectKind::is_relation)
        );
        assert!(
            ![
                ObjectKind::Index,
                ObjectKind::Constraint,
                ObjectKind::Trigger,
                ObjectKind::Function,
                ObjectKind::Procedure
            ]
            .iter()
            .any(ObjectKind::is_relation)
        );
    }
}
