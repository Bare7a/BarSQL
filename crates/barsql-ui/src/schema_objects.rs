use barsql_core::{ConstraintInfo, IndexInfo, ObjectKind, ObjectRef, RoutineInfo, TriggerInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    Pk,
    Fk,
    Unique,
    Check,
    Index,
    Function,
    Procedure,
}

impl Badge {
    pub fn key(self) -> &'static str {
        match self {
            Self::Pk => "pk",
            Self::Fk => "fk",
            Self::Unique => "unique",
            Self::Check => "check",
            Self::Index => "index",
            Self::Function => "function",
            Self::Procedure => "procedure",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectRow {
    // Unique within its group, unlike the name.
    pub key: String,
    pub label: String,
    pub detail: String,
    pub badge: Option<Badge>,
    pub object: ObjectRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    Indexes,
    Constraints,
    Triggers,
    Routines,
}

impl Group {
    pub const TABLE: [Group; 3] = [Group::Indexes, Group::Constraints, Group::Triggers];

    pub fn key(self) -> &'static str {
        match self {
            Self::Indexes => "indexes",
            Self::Constraints => "constraints",
            Self::Triggers => "triggers",
            Self::Routines => "routines",
        }
    }
}

pub fn group_key(connection_id: &str, schema: &str, table: &str, group: Group) -> String {
    format!("{connection_id}:{schema}:{table}:{}", group.key())
}

pub fn routines_key(connection_id: &str, schema: &str) -> String {
    format!("{connection_id}:{schema}:routines")
}

fn columns(list: &[String]) -> String {
    if list.is_empty() { String::new() } else { format!("({})", list.join(", ")) }
}

fn object(schema: String, name: String, kind: ObjectKind, table: String) -> ObjectRef {
    ObjectRef { schema, name, kind, table, args: String::new() }
}

pub fn index_rows(indexes: Vec<IndexInfo>) -> Vec<ObjectRow> {
    indexes
        .into_iter()
        .map(|idx| ObjectRow {
            key: idx.name.clone(),
            label: idx.name.clone(),
            detail: columns(&idx.columns),
            badge: Some(if idx.is_primary {
                Badge::Pk
            } else if idx.is_unique {
                Badge::Unique
            } else {
                Badge::Index
            }),
            object: object(idx.schema, idx.name, ObjectKind::Index, idx.table),
        })
        .collect()
}

fn constraint_badge(kind: &str) -> Option<Badge> {
    match kind.to_uppercase().as_str() {
        "PRIMARY KEY" => Some(Badge::Pk),
        "FOREIGN KEY" => Some(Badge::Fk),
        "UNIQUE" => Some(Badge::Unique),
        "CHECK" => Some(Badge::Check),
        _ => None,
    }
}

fn constraint_detail(c: &ConstraintInfo) -> String {
    match c.kind.to_uppercase().as_str() {
        "FOREIGN KEY" if !c.ref_table.is_empty() => {
            format!("{} → {}{}", columns(&c.columns), c.ref_table, columns(&c.ref_columns))
        }
        "CHECK" => c.definition.clone(),
        _ => columns(&c.columns),
    }
}

pub fn constraint_rows(constraints: Vec<ConstraintInfo>) -> Vec<ObjectRow> {
    constraints
        .into_iter()
        .map(|c| ObjectRow {
            // Two unnamed constraints of one kind cannot cover the same columns.
            key: if c.name.is_empty() { format!("{}:{}", c.kind, c.columns.join(",")) } else { c.name.clone() },
            // SQLite's inline keys arrive unnamed.
            label: if c.name.is_empty() { c.kind.clone() } else { c.name.clone() },
            detail: constraint_detail(&c),
            badge: constraint_badge(&c.kind),
            object: object(c.schema, c.name, ObjectKind::Constraint, c.table),
        })
        .collect()
}

pub fn trigger_rows(triggers: Vec<TriggerInfo>) -> Vec<ObjectRow> {
    triggers
        .into_iter()
        .map(|tr| ObjectRow {
            key: tr.name.clone(),
            label: tr.name.clone(),
            detail: [tr.timing.as_str(), tr.events.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            badge: None,
            object: object(tr.schema, tr.name, ObjectKind::Trigger, tr.table),
        })
        .collect()
}

pub fn routine_rows(routines: Vec<RoutineInfo>) -> Vec<ObjectRow> {
    routines
        .into_iter()
        .map(|r| ObjectRow {
            key: if r.args.is_empty() { r.name.clone() } else { format!("{}({})", r.name, r.args) },
            label: format!("{}({})", r.name, r.args),
            detail: if r.return_type.is_empty() { String::new() } else { format!("→ {}", r.return_type) },
            badge: Some(if r.kind == ObjectKind::Procedure { Badge::Procedure } else { Badge::Function }),
            object: ObjectRef { schema: r.schema, name: r.name, kind: r.kind, table: String::new(), args: r.args },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(name: &str, columns: &[&str], is_primary: bool, is_unique: bool) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            schema: "public".into(),
            table: "users".into(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            is_primary,
            is_unique,
            ..Default::default()
        }
    }

    fn constraint(name: &str, kind: &str, columns: &[&str]) -> ConstraintInfo {
        ConstraintInfo {
            name: name.into(),
            schema: "public".into(),
            table: "users".into(),
            kind: kind.into(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn index_rows_render_columns_and_carry_a_ddl_ref() {
        let row = index_rows(vec![index("users_email_idx", &["email", "org_id"], false, false)]).remove(0);
        assert_eq!((row.label.as_str(), row.detail.as_str()), ("users_email_idx", "(email, org_id)"));
        assert_eq!(row.object, object("public".into(), "users_email_idx".into(), ObjectKind::Index, "users".into()));
    }

    #[test]
    fn index_badges_put_primary_before_unique() {
        assert_eq!(index_rows(vec![index("i", &["a"], true, true)])[0].badge, Some(Badge::Pk));
        assert_eq!(index_rows(vec![index("i", &["a"], false, true)])[0].badge, Some(Badge::Unique));
        assert_eq!(index_rows(vec![index("i", &["a"], false, false)])[0].badge, Some(Badge::Index));
        assert_eq!(index_rows(vec![index("i", &[], false, false)])[0].detail, "");
    }

    #[test]
    fn foreign_keys_point_at_their_target_or_fall_back_to_local_columns() {
        let fk = ConstraintInfo {
            ref_table: "orgs".into(),
            ref_columns: vec!["id".into()],
            ..constraint("users_org_fk", "FOREIGN KEY", &["org_id"])
        };
        let row = constraint_rows(vec![fk]).remove(0);
        assert_eq!((row.detail.as_str(), row.badge), ("(org_id) → orgs(id)", Some(Badge::Fk)));
        assert_eq!(row.object.kind, ObjectKind::Constraint);
        assert_eq!(constraint_rows(vec![constraint("c", "FOREIGN KEY", &["org_id"])])[0].detail, "(org_id)");
    }

    #[test]
    fn checks_show_their_body_and_unknown_kinds_get_no_badge() {
        let check = ConstraintInfo { definition: "CHECK ((age > 0))".into(), ..constraint("c", "CHECK", &[]) };
        let row = constraint_rows(vec![check]).remove(0);
        assert_eq!((row.detail.as_str(), row.badge), ("CHECK ((age > 0))", Some(Badge::Check)));
        assert_eq!(constraint_rows(vec![constraint("c", "PRIMARY KEY", &["a", "b"])])[0].detail, "(a, b)");
        assert_eq!(constraint_rows(vec![constraint("c", "UNIQUE", &["a"])])[0].badge, Some(Badge::Unique));
        assert_eq!(constraint_rows(vec![constraint("c", "EXCLUDE", &["a"])])[0].badge, None);
    }

    #[test]
    fn unnamed_sqlite_constraints_are_labelled_and_keyed_apart() {
        let orgs = ConstraintInfo { ref_table: "orgs".into(), ..constraint("", "FOREIGN KEY", &["org_id"]) };
        let rows = constraint_rows(vec![constraint("", "PRIMARY KEY", &["id"]), orgs]);
        assert_eq!(rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["PRIMARY KEY", "FOREIGN KEY"]);
        assert_ne!(rows[0].key, rows[1].key);
        assert_eq!(rows[0].object.name, "");
    }

    #[test]
    fn triggers_join_timing_and_events() {
        let trigger = TriggerInfo {
            name: "users_audit".into(),
            schema: "public".into(),
            table: "users".into(),
            timing: "AFTER".into(),
            events: "INSERT, UPDATE".into(),
        };
        let row = trigger_rows(vec![trigger]).remove(0);
        assert_eq!(row.detail, "AFTER INSERT, UPDATE");
        assert_eq!(row.object, object("public".into(), "users_audit".into(), ObjectKind::Trigger, "users".into()));
        let no_timing =
            TriggerInfo { name: "t".into(), schema: "main".into(), events: "UPDATE".into(), ..Default::default() };
        assert_eq!(trigger_rows(vec![no_timing])[0].detail, "UPDATE");
    }

    #[test]
    fn routines_show_signatures_and_key_overloads_apart() {
        let routine = |args: &str, kind: ObjectKind| RoutineInfo {
            name: "add".into(),
            schema: "public".into(),
            kind,
            args: args.into(),
            ..Default::default()
        };
        let typed =
            RoutineInfo { return_type: "integer".into(), ..routine("a integer, b integer", ObjectKind::Function) };
        let row = routine_rows(vec![typed]).remove(0);
        assert_eq!((row.label.as_str(), row.detail.as_str()), ("add(a integer, b integer)", "→ integer"));
        assert_eq!(row.badge, Some(Badge::Function));
        let rows =
            routine_rows(vec![routine("a integer", ObjectKind::Function), routine("a text", ObjectKind::Function)]);
        assert_eq!(rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["add(a integer)", "add(a text)"]);
        assert_eq!(rows[0].object.args, "a integer");
        let procedure =
            routine_rows(vec![RoutineInfo { name: "cleanup".into(), ..routine("", ObjectKind::Procedure) }]);
        assert_eq!((procedure[0].label.as_str(), procedure[0].detail.as_str()), ("cleanup()", ""));
        assert_eq!(procedure[0].badge, Some(Badge::Procedure));
    }

    #[test]
    fn cache_keys_separate_groups_and_scope_routines() {
        assert_eq!(group_key("c1", "public", "users", Group::Indexes), "c1:public:users:indexes");
        assert_ne!(
            group_key("c1", "public", "users", Group::Triggers),
            group_key("c1", "public", "users", Group::Indexes)
        );
        assert_eq!(routines_key("c1", "public"), "c1:public:routines");
    }
}
