use barsql_core::{DriverType, Identity, PrimaryKey, StructColumn, StructIndex, TableStructure};

use super::{Changes, Draft, NameKind, check_clause, ddl_column, suggested_name};
use crate::alter::{drop_column, rename_column};
use crate::ddl::compose_create_table;
use crate::quote::{quote_ident, quote_ident_list, table_ref};

// SQL Server keeps a default as a named constraint, and won't change a column's type while one depends on it.
pub(super) fn alter(driver: &DriverType, original: &TableStructure, changes: &Changes) -> Vec<String> {
    let (schema, name) = (original.schema.as_str(), original.name.as_str());
    let table = table_ref(driver, schema, name);
    let constraint =
        |what: &str, name: &str| format!("ALTER TABLE {table} {what} CONSTRAINT {}", quote_ident(driver, name));
    let mut out = Vec::new();
    if let Some((Some(old), _)) = &changes.primary_key
        && !old.name.is_empty()
    {
        out.push(constraint("DROP", &old.name));
    }
    // An index blocks changes to the columns it covers, so it goes first and comes back last.
    for index in &changes.dropped_indexes {
        out.push(match index.constraint {
            true => constraint("DROP", &index.name),
            false => format!("DROP INDEX {} ON {table}", quote_ident(driver, &index.name)),
        });
    }
    out.extend(changes.dropped_checks.iter().map(|check| constraint("DROP", &check.name)));
    for (old, new) in &changes.renamed {
        out.push(rename_column(driver, schema, name, old, new));
    }
    for (old, new) in &changes.modified {
        let column = quote_ident(driver, &new.name);
        let retyped = old.data_type != new.data_type || old.nullable != new.nullable || old.collation != new.collation;
        let has_default = !old.default.is_empty() && !old.default_name.is_empty();
        let redefault = old.default != new.default || (has_default && retyped);
        if has_default && redefault {
            out.push(constraint("DROP", &old.default_name));
        }
        if retyped {
            out.push(format!("ALTER TABLE {table} ALTER COLUMN {column} {}", type_and_null(new)));
        }
        if redefault && !new.default.is_empty() {
            let default_name = match old.default_name.as_str() {
                "" => suggested_name(driver, NameKind::Default, name, std::slice::from_ref(&new.name), ""),
                kept => kept.to_string(),
            };
            out.push(format!("{} DEFAULT {} FOR {column}", constraint("ADD", &default_name), new.default));
        }
    }
    for column in &changes.dropped {
        if !column.default_name.is_empty() {
            out.push(constraint("DROP", &column.default_name));
        }
        out.push(drop_column(driver, schema, name, &column.name, false));
    }
    for column in &changes.added {
        out.push(format!("ALTER TABLE {table} ADD {}", added_column(driver, name, column)));
    }
    if let Some((_, Some(key))) = &changes.primary_key {
        out.push(format!("ALTER TABLE {table} ADD {}", primary_key(driver, name, key)));
    }
    out.extend(changes.added_indexes.iter().map(|index| create_index(driver, &table, index)));
    out.extend(
        changes.added_checks.iter().map(|check| format!("ALTER TABLE {table} ADD {}", check_clause(driver, check))),
    );
    out
}

fn create_index(driver: &DriverType, table: &str, index: &StructIndex) -> String {
    let (name, columns) = (quote_ident(driver, &index.name), quote_ident_list(driver, &index.columns));
    if index.constraint && index.unique {
        return format!("ALTER TABLE {table} ADD CONSTRAINT {name} UNIQUE ({columns})");
    }
    let unique = if index.unique { "UNIQUE " } else { "" };
    let clustered = if index.method.eq_ignore_ascii_case("clustered") { "CLUSTERED " } else { "" };
    format!("CREATE {unique}{clustered}INDEX {name} ON {table} ({columns})")
}

pub(super) fn create(driver: &DriverType, draft: &Draft) -> Vec<String> {
    let columns: Vec<_> = draft.columns.iter().filter(|row| !row.dropped).map(|row| ddl_column(&row.column)).collect();
    let keys = draft.primary_columns();
    let constraints = match keys.is_empty() {
        true => Vec::new(),
        false => vec![primary_key(driver, &draft.name, &PrimaryKey { name: String::new(), columns: keys })],
    };
    let create = compose_create_table(driver, &draft.schema, &draft.name, &columns, &constraints);
    vec![create.trim_end_matches(';').to_string()]
}

fn type_and_null(column: &StructColumn) -> String {
    let mut out = column.data_type.clone();
    if !column.collation.is_empty() {
        out.push_str(&format!(" COLLATE {}", column.collation));
    }
    out.push_str(if column.nullable { " NULL" } else { " NOT NULL" });
    out
}

fn added_column(driver: &DriverType, table: &str, column: &StructColumn) -> String {
    let mut out = format!("{} {}", quote_ident(driver, &column.name), column.data_type);
    if !column.collation.is_empty() {
        out.push_str(&format!(" COLLATE {}", column.collation));
    }
    if let Identity::Seeded { seed, increment } = column.identity {
        out.push_str(&format!(" IDENTITY({seed}, {increment})"));
    }
    if !column.default.is_empty() {
        let name = suggested_name(driver, NameKind::Default, table, std::slice::from_ref(&column.name), "");
        out.push_str(&format!(" CONSTRAINT {} DEFAULT {}", quote_ident(driver, &name), column.default));
    }
    out.push_str(if column.nullable { " NULL" } else { " NOT NULL" });
    out
}

fn primary_key(driver: &DriverType, table: &str, key: &PrimaryKey) -> String {
    let name = match key.name.as_str() {
        "" => suggested_name(driver, NameKind::PrimaryKey, table, &[], ""),
        name => name.to_string(),
    };
    format!("CONSTRAINT {} PRIMARY KEY ({})", quote_ident(driver, &name), quote_ident_list(driver, &key.columns))
}

#[cfg(test)]
mod tests {
    use barsql_core::{Identity, StructColumn, StructIndex, TableStructure};

    use crate::structure::fixtures::*;
    use crate::structure::{Draft, Row, plan_changes, plan_create};

    fn users() -> TableStructure {
        TableStructure {
            schema: "dbo".into(),
            name: "users".into(),
            columns: vec![
                StructColumn {
                    nullable: false,
                    identity: Identity::Seeded { seed: 1, increment: 1 },
                    ..column("id", "int")
                },
                column("name", "nvarchar(50)"),
                StructColumn {
                    default: "(N'new')".into(),
                    default_name: "DF__users__status__1A2B".into(),
                    ..column("status", "nvarchar(20)")
                },
                StructColumn {
                    default: "((0))".into(),
                    default_name: "DF_users_legacy".into(),
                    ..column("legacy", "nvarchar(10)")
                },
            ],
            primary_key: Some(pk("PK_users", &["id"])),
            ..Default::default()
        }
    }

    #[test]
    fn sql_server_alters_through_defaults_and_renames() {
        let original = users();
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(1, "full_name".into());
        draft.columns[1].column.data_type = "nvarchar(100)".into();
        draft.columns[2].column.data_type = "nvarchar(40)".into();
        draft.columns[2].column.default = "N'active'".into();
        draft.columns[3].dropped = true;
        draft.columns.push(added(StructColumn { nullable: false, default: "0".into(), ..column("age", "int") }));
        let plan = plan_changes(&MS, &original, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "SET XACT_ABORT ON",
                "BEGIN TRANSACTION",
                "EXEC sp_rename N'[dbo].[users].[name]', N'full_name', 'COLUMN'",
                "ALTER TABLE [dbo].[users] ALTER COLUMN [full_name] nvarchar(100) NULL",
                "ALTER TABLE [dbo].[users] DROP CONSTRAINT [DF__users__status__1A2B]",
                "ALTER TABLE [dbo].[users] ALTER COLUMN [status] nvarchar(40) NULL",
                "ALTER TABLE [dbo].[users] ADD CONSTRAINT [DF__users__status__1A2B] DEFAULT N'active' FOR [status]",
                "ALTER TABLE [dbo].[users] DROP CONSTRAINT [DF_users_legacy]",
                "ALTER TABLE [dbo].[users] DROP COLUMN [legacy]",
                "ALTER TABLE [dbo].[users] ADD [age] int CONSTRAINT [DF_users_age] DEFAULT 0 NOT NULL",
                "COMMIT TRANSACTION",
            ]
        );
    }

    #[test]
    fn sql_server_adds_and_drops_indexes() {
        let mut original = users();
        original.indexes = vec![
            StructIndex { method: "nonclustered".into(), ..index("IX_users_name", &["name"], false) },
            StructIndex { constraint: true, ..index("UQ_users_status", &["status"], true) },
        ];
        let mut draft = Draft::from_structure(&original);
        draft.indexes[0].dropped = true;
        draft.indexes[1].dropped = true;
        draft.columns.push(added(column("email", "nvarchar(320)")));
        draft.indexes.push(Row::added(index("IX_users_email", &["email"], true)));
        let plan = plan_changes(&MS, &original, &draft).expect("valid");
        let steps = sql(&plan);
        assert_eq!(
            steps[2..steps.len() - 1],
            [
                "DROP INDEX [IX_users_name] ON [dbo].[users]",
                "ALTER TABLE [dbo].[users] DROP CONSTRAINT [UQ_users_status]",
                "ALTER TABLE [dbo].[users] ADD [email] nvarchar(320) NULL",
                "CREATE UNIQUE INDEX [IX_users_email] ON [dbo].[users] ([email])",
            ]
        );
    }

    #[test]
    fn sql_server_keeps_a_clustered_index_clustered() {
        let mut original = users();
        original.indexes =
            vec![StructIndex { method: "clustered".into(), ..index("IX_users_status", &["status"], false) }];
        let mut draft = Draft::from_structure(&original);
        draft.indexes[0].value.unique = true;
        let plan = plan_changes(&MS, &original, &draft).expect("valid");
        let steps = sql(&plan);
        assert_eq!(
            steps[2..steps.len() - 1],
            [
                "DROP INDEX [IX_users_status] ON [dbo].[users]",
                "CREATE UNIQUE CLUSTERED INDEX [IX_users_status] ON [dbo].[users] ([status])",
            ]
        );
    }

    #[test]
    fn sql_server_adds_and_drops_checks() {
        let mut original = users();
        original.checks = vec![check("CK_users_status", "[status]<>N''")];
        let mut draft = Draft::from_structure(&original);
        draft.checks[0].dropped = true;
        draft.checks.push(Row::added(check("CK_users_name", "len(name) > 0")));
        draft.checks.push(Row::added(check("", "id > 0")));
        let plan = plan_changes(&MS, &original, &draft).expect("valid");
        let steps = sql(&plan);
        assert_eq!(
            steps[2..steps.len() - 1],
            [
                "ALTER TABLE [dbo].[users] DROP CONSTRAINT [CK_users_status]",
                "ALTER TABLE [dbo].[users] ADD CONSTRAINT [CK_users_name] CHECK (len(name) > 0)",
                "ALTER TABLE [dbo].[users] ADD CHECK (id > 0)",
            ]
        );
    }

    #[test]
    fn sql_server_quotes_brackets() {
        let mut original = users();
        original.columns.push(column("a]b", "int"));
        let mut draft = Draft::from_structure(&original);
        draft.rename_column(4, "ab".into());
        let plan = plan_changes(&MS, &original, &draft).expect("valid");
        assert!(sql(&plan).iter().any(|step| step.contains("N'[dbo].[users].[a]]b]'")), "{:?}", sql(&plan));
    }

    #[test]
    fn sql_server_creates_a_table() {
        let mut draft = Draft::new_table(&MS, "dbo");
        draft.name = "people".into();
        let plan = plan_create(&MS, &draft).expect("valid");
        assert_eq!(
            sql(&plan),
            [
                "SET XACT_ABORT ON",
                "BEGIN TRANSACTION",
                "CREATE TABLE [dbo].[people] (\n    [id] bigint IDENTITY(1, 1) NOT NULL,\n    CONSTRAINT [PK_people] PRIMARY KEY ([id])\n)",
                "COMMIT TRANSACTION",
            ]
        );
    }
}
