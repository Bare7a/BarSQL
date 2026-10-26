use serde::{Deserialize, Serialize};

// A table's shape as the structure editor reads and writes it.

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableStructure {
    pub schema: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<StructColumn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<PrimaryKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<StructIndex>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    // SQLite: the table's own CREATE TABLE, which a rebuild copies untouched items from.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub create_sql: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependents: Vec<Dependent>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sorting_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructColumn {
    pub name: String,
    pub data_type: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default: String,
    #[serde(default, skip_serializing_if = "Identity::is_none")]
    pub identity: Identity,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub generated: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub collation: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
    // Kept verbatim, like MySQL's `on update CURRENT_TIMESTAMP`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub extra: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_name: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Identity {
    #[default]
    None,
    ByDefault,
    Always,
    AutoIncrement,
    Seeded {
        seed: i64,
        increment: i64,
    },
}

impl Identity {
    pub fn is_none(&self) -> bool {
        *self == Self::None
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryKey {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructIndex {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub method: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub constraint: bool,
    // Set for an index the editor can't model, like an expression or partial one. It can be dropped, not edited.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub definition: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKey {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ref_schema: String,
    pub ref_table: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ref_columns: Vec<String>,
    #[serde(default)]
    pub on_update: RefAction,
    #[serde(default)]
    pub on_delete: RefAction,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RefAction {
    #[default]
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl RefAction {
    pub fn sql(self) -> &'static str {
        match self {
            Self::NoAction => "NO ACTION",
            Self::Restrict => "RESTRICT",
            Self::Cascade => "CASCADE",
            Self::SetNull => "SET NULL",
            Self::SetDefault => "SET DEFAULT",
        }
    }

    // Takes SQL Server's SET_NULL and Postgres' one-letter confdeltype codes too.
    pub fn parse(text: &str) -> Self {
        match text.trim().to_ascii_uppercase().replace('_', " ").as_str() {
            "RESTRICT" | "R" => Self::Restrict,
            "CASCADE" | "C" => Self::Cascade,
            "SET NULL" | "N" => Self::SetNull,
            "SET DEFAULT" | "D" => Self::SetDefault,
            _ => Self::NoAction,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub expression: String,
}

// An index or trigger that a SQLite rebuild recreates from its stored SQL.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dependent {
    pub kind: String,
    pub name: String,
    pub sql: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_actions_parse_and_print() {
        assert_eq!(RefAction::parse("set_null"), RefAction::SetNull);
        assert_eq!(RefAction::parse("SET DEFAULT"), RefAction::SetDefault);
        assert_eq!(RefAction::parse("c"), RefAction::Cascade);
        assert_eq!(RefAction::parse("bogus"), RefAction::NoAction);
        assert_eq!(RefAction::SetNull.sql(), "SET NULL");
    }

    #[test]
    fn structures_serialize_without_empty_fields() {
        let column = StructColumn { name: "id".into(), data_type: "int".into(), ..Default::default() };
        assert_eq!(serde_json::to_string(&column).unwrap(), r#"{"name":"id","dataType":"int"}"#);

        let full = TableStructure {
            schema: "public".into(),
            name: "users".into(),
            columns: vec![
                StructColumn {
                    nullable: true,
                    default: "'new'".into(),
                    identity: Identity::Seeded { seed: 1, increment: 2 },
                    collation: "C".into(),
                    comment: "Login".into(),
                    extra: "on update CURRENT_TIMESTAMP".into(),
                    default_name: "DF_users_id".into(),
                    ..column.clone()
                },
                StructColumn { generated: "a * 2".into(), identity: Identity::Always, ..column },
            ],
            primary_key: Some(PrimaryKey { name: "users_pkey".into(), columns: vec!["id".into()] }),
            indexes: vec![StructIndex {
                name: "users_lower".into(),
                columns: vec!["email".into()],
                unique: true,
                method: "btree".into(),
                constraint: true,
                definition: "CREATE INDEX users_lower ON users (lower(email))".into(),
            }],
            foreign_keys: vec![ForeignKey {
                name: "users_org_fkey".into(),
                columns: vec!["org_id".into()],
                ref_schema: "public".into(),
                ref_table: "orgs".into(),
                ref_columns: vec!["id".into()],
                on_update: RefAction::Cascade,
                on_delete: RefAction::SetNull,
            }],
            checks: vec![Check { name: "age_ok".into(), expression: "age >= 0".into() }],
            create_sql: "CREATE TABLE users (id int)".into(),
            dependents: vec![Dependent {
                kind: "trigger".into(),
                name: "users_touch".into(),
                sql: "CREATE TRIGGER users_touch AFTER UPDATE ON users BEGIN SELECT 1; END".into(),
            }],
            sorting_key: "id".into(),
        };
        let json = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<TableStructure>(&json).unwrap(), full);
    }
}
