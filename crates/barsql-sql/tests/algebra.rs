use barsql_core::SqlDialect;
use barsql_sql::{AlgebraError, AlgebraNode, algebra, columns};

fn show(node: &AlgebraNode) -> String {
    let head =
        [node.symbol, node.detail.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
    match node.children.is_empty() {
        true => head,
        false => format!("{head}({})", node.children.iter().map(show).collect::<Vec<_>>().join(", ")),
    }
}

fn tree(sql: &str) -> String {
    show(&algebra(sql, SqlDialect::Postgres).unwrap())
}

#[test]
fn join_filter_group_sort_limit() {
    assert_eq!(
        tree(
            "SELECT u.name, count(*) FROM users u LEFT JOIN orders o ON o.user_id = u.id WHERE u.active GROUP BY u.name HAVING count(*) > 1 ORDER BY 2 DESC LIMIT 5 OFFSET 10"
        ),
        "limit 5 OFFSET 10(τ 2 DESC(π u.name, count(*)(σ count(*) > 1(γ u.name(σ u.active(⋈ LEFT ON o.user_id = u.id(users u, orders o)))))))"
    );
}

#[test]
fn cte_is_inlined_and_set_operations_branch() {
    assert_eq!(
        tree("WITH a AS (SELECT id FROM t) SELECT id FROM a UNION ALL SELECT id FROM b"),
        "∪ ALL(π id(π id(t)), π id(b))"
    );
}

#[test]
fn subquery_alias_and_distinct() {
    assert_eq!(tree("SELECT DISTINCT x FROM (SELECT 1 AS x) s, t"), "δ(π x(×(ρ s(π 1 AS x(())), t)))");
}

#[test]
fn rejects_what_is_not_a_query() {
    assert_eq!(algebra("DELETE FROM t", SqlDialect::Sqlite), Err(AlgebraError::NotQuery));
    assert_eq!(algebra("SELECT 1; SELECT 2", SqlDialect::Sqlite), Err(AlgebraError::NotQuery));
    assert!(matches!(algebra("SELEC 1", SqlDialect::Sqlite), Err(AlgebraError::Parse(_))));
}

fn find<'a>(node: &'a AlgebraNode, symbol: &str) -> &'a AlgebraNode {
    match node.symbol == symbol {
        true => node,
        false => node.children.iter().map(|c| find(c, symbol)).find(|n| n.symbol == symbol).expect(symbol),
    }
}

#[test]
fn operators_point_at_their_text() {
    let sql = "SELECT u.name, count(*) AS n\nFROM users u JOIN orders o ON o.user_id = u.id\nWHERE u.active\nORDER BY n DESC LIMIT 5";
    let root = algebra(sql, SqlDialect::Postgres).unwrap();
    let text = |symbol: &str| &sql[find(&root, symbol).span.clone().unwrap()];
    assert_eq!(text("σ"), "u.active");
    assert_eq!(text("π"), "u.name, count(*) AS n");
    assert_eq!(text("⋈"), "orders o ON o.user_id = u.id");
    assert_eq!(text("τ"), "n");
    assert_eq!(text("limit"), "5");
}

#[test]
fn projections_list_their_items_by_alias() {
    let root = algebra("SELECT a.x, count(*) AS n, b.* FROM a, b", SqlDialect::Sqlite).unwrap();
    let labels: Vec<_> = find(&root, "π").items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["a.x", "n", "b.*"]);
}

#[test]
fn columns_follow_the_operators() {
    let lookup = |table: &str| match table {
        "users" => Some(vec![("id".to_string(), "integer".to_string()), ("name".to_string(), "text".to_string())]),
        "orders" => Some(vec![("id".to_string(), "integer".to_string()), ("total".to_string(), "real".to_string())]),
        _ => None,
    };
    let names = |sql: &str, symbol: &str| {
        let root = algebra(sql, SqlDialect::Sqlite).unwrap();
        columns(find(&root, symbol), &lookup).into_iter().map(|c| format!("{}.{}", c.table, c.name)).collect::<Vec<_>>()
    };
    let sql = "SELECT u.name, o.total AS t, count(*) FROM users u JOIN orders o ON o.id = u.id";
    assert_eq!(names(sql, "⋈"), ["u.id", "u.name", "o.id", "o.total"]);
    assert_eq!(names(sql, "π"), ["u.name", "o.t", ".count(*)"]);
}
