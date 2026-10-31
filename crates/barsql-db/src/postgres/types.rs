use barsql_core::DriverType;
use barsql_sql::lang::quoting::format_sql_identifier;
use tokio_postgres::types::{Kind, Type};

// A built-in type by its internal name in capitals, like INT4, or _TEXT for an array. Any other, like an enum or an
// extension's type, as SQL writes its name, so "Mood" keeps its case.
pub fn type_name(ty: &Type) -> String {
    if ty.schema() == "pg_catalog" {
        return ty.name().to_ascii_uppercase();
    }
    match ty.kind() {
        Kind::Array(element) => format!("{}[]", type_name(element)),
        _ => format_sql_identifier(ty.name(), &DriverType::Postgres),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_types_by_internal_name_others_as_sql_writes_them() {
        let names =
            [Type::INT4, Type::BPCHAR, Type::CHAR, Type::INT4_ARRAY, Type::TIMETZ, Type::MONEY].map(|t| type_name(&t));
        assert_eq!(names, ["INT4", "BPCHAR", "CHAR", "_INT4", "TIMETZ", "MONEY"]);
        let user = |name: &str, kind| Type::new(name.into(), 16_384, kind, "public".into());
        let pascal = user("TransactionType", Kind::Enum(vec!["refund".into()]));
        assert_eq!(type_name(&user("mood", Kind::Enum(Vec::new()))), "mood");
        assert_eq!(type_name(&pascal), "\"TransactionType\"");
        assert_eq!(type_name(&user("_TransactionType", Kind::Array(pascal))), "\"TransactionType\"[]");
        assert_eq!(type_name(&user("citext", Kind::Simple)), "citext");
    }
}
