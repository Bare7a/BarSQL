use barsql_core::{DriverType, FunctionInfo, FunctionKind, FunctionList, FunctionSignature};

use super::*;

const ALL: [(&str, &[BuiltinFn]); 7] = [
    ("core", core::PACK),
    ("postgres", postgres::PACK),
    ("mysql", mysql::PACK),
    ("sqlite", sqlite::PACK),
    ("libsql", libsql::PACK),
    ("tsql", tsql::PACK),
    ("clickhouse", clickhouse::PACK),
];

#[test]
fn packs_are_sorted_unique_and_documented() {
    for (name, pack) in ALL {
        for pair in pack.windows(2) {
            let (a, b) = (pair[0].name.to_ascii_lowercase(), pair[1].name.to_ascii_lowercase());
            assert!(a <= b, "{name}: {} sorts after {}", pair[0].name, pair[1].name);
            // ClickHouse has names that differ only in case, like toDate and todate's aliases.
            let same = if name == "clickhouse" { pair[0].name == pair[1].name } else { a == b };
            assert!(!same, "{name}: {} is listed twice", pair[1].name);
        }
        for f in pack {
            assert!(
                !f.summary.is_empty() && f.summary.ends_with('.'),
                "{name}: {} needs a one-sentence summary",
                f.name
            );
            assert!(!f.sigs.is_empty(), "{name}: {} has no signature", f.name);
            for s in f.sigs {
                assert!(
                    !s.args.starts_with('(') && !s.args.ends_with(')') || s.args.contains(") "),
                    "{name}: {} args keep their parentheses: {}",
                    f.name,
                    s.args
                );
            }
            if f.has(flags::NO_PARENS) {
                assert!(f.sigs.iter().all(|s| s.args.is_empty()), "{name}: {} takes no arguments", f.name);
            }
        }
    }
}

#[test]
fn shared_functions_resolve_everywhere() {
    for driver in DriverType::KNOWN {
        let catalog = FunctionCatalog::builtin(&driver);
        for f in core::PACK {
            assert!(catalog.lookup(None, f.name).is_some(), "{driver}: {}", f.name);
            assert!(catalog.lookup(None, &f.name.to_ascii_uppercase()).is_some(), "{driver}: {} in uppercase", f.name);
        }
    }
    let tsql = FunctionCatalog::builtin(&DriverType::SqlServer);
    assert_eq!(tsql.lookup(None, "count").unwrap().name, "COUNT");
    assert_eq!(FunctionCatalog::builtin(&DriverType::Postgres).lookup(None, "COUNT").unwrap().name, "count");
}

fn server(name: &str, schema: &str, builtin: bool, kind: FunctionKind) -> FunctionInfo {
    FunctionInfo {
        name: name.into(),
        schema: schema.into(),
        kind,
        builtin,
        signatures: vec![FunctionSignature { args: "x int".into(), returns: "int".into() }],
        ..Default::default()
    }
}

#[test]
fn the_server_catalog_adds_and_hides_functions() {
    let functions = vec![
        server("count", "pg_catalog", true, FunctionKind::Aggregate),
        server("lower", "pg_catalog", true, FunctionKind::Scalar),
        server("brand_new", "pg_catalog", true, FunctionKind::Scalar),
        server("tax", "billing", false, FunctionKind::Scalar),
    ];
    let catalog = FunctionCatalog::with_server(&DriverType::Postgres, FunctionList { functions, ..Default::default() });
    // A live list hides what the server lacks, but keeps special forms.
    assert!(catalog.lookup(None, "upper").is_none());
    assert!(catalog.lookup(None, "cast").is_some());
    assert!(catalog.lookup(None, "brand_new").is_some_and(|d| d.builtin));
    let tax = catalog.lookup(Some("billing"), "tax").expect("user function");
    assert_eq!((tax.schema.as_str(), tax.builtin), ("billing", false));
    let found = catalog.matches("ta", KindMask::EXPR, 10);
    assert_eq!(found.first().map(|m| (m.0, m.2.name.as_str())), Some((TIER_USER, "tax")));
    assert!(catalog.matches("brand", KindMask::EXPR, 10).iter().any(|m| m.0 == TIER_SERVER));
    assert!(catalog.matches("count", KindMask::SCALAR, 10).is_empty(), "count is an aggregate");
}

#[test]
fn clickhouse_names_keep_their_case_except_standard_ones() {
    let catalog = FunctionCatalog::builtin(&DriverType::ClickHouse);
    assert!(catalog.lookup(None, "COUNT").is_some());
    assert!(catalog.lookup(None, "Sum").is_some());
}

fn chain(doc: &FunctionDoc) -> Option<(&str, Vec<&str>)> {
    doc.combinator.as_ref().map(|(base, chain)| (base.as_str(), chain.iter().map(|(s, _)| s.as_str()).collect()))
}

// The pack lists common forms like sumIf on their own, so these use forms it doesn't.
#[test]
fn clickhouse_combinators_resolve_to_their_base() {
    let catalog = FunctionCatalog::builtin(&DriverType::ClickHouse);
    let doc = catalog.lookup(None, "maxOrNullIf").expect("max + OrNull + If");
    assert_eq!((doc.name.as_str(), doc.kind), ("maxOrNullIf", FunctionKind::Aggregate));
    assert_eq!(chain(&doc), Some(("max", vec!["OrNull", "If"])));
    assert!(doc.combinator.unwrap().1.iter().all(|(_, about)| !about.is_empty()));
    assert_eq!(chain(&catalog.lookup(None, "minMergeState").unwrap()), Some(("min", vec!["MergeState"])));
    assert!(catalog.lookup(None, "lowerIf").is_none(), "lower isn't an aggregate");
    assert!(catalog.lookup(None, "anyIfIfIfIf").is_none(), "at most three suffixes");
    assert!(FunctionCatalog::builtin(&DriverType::Postgres).lookup(None, "maxIf").is_none());
}

#[test]
fn mysql_and_mariadb_only_offer_their_own_functions() {
    const MYSQL: BuiltinFn = scalar("only_mysql", &[sig("", "int")], "MySQL.").flags(flags::MYSQL_ONLY);
    const MARIADB: BuiltinFn = scalar("only_mariadb", &[sig("", "int")], "MariaDB.").flags(flags::MARIADB_ONLY);
    let on = |mariadb: Option<bool>| {
        let catalog = match mariadb {
            None => FunctionCatalog::builtin(&DriverType::MySql),
            Some(mariadb) => {
                FunctionCatalog::with_server(&DriverType::MySql, FunctionList { mariadb, ..Default::default() })
            }
        };
        (catalog.offered(&MYSQL), catalog.offered(&MARIADB))
    };
    assert_eq!(on(None), (true, true), "an unknown server offers both");
    assert_eq!(on(Some(false)), (true, false));
    assert_eq!(on(Some(true)), (false, true));
}

// Extensions rank with the built-ins. An overload of an offered name doesn't repeat it, unless it needs its schema.
#[test]
fn extension_functions_and_overloads_rank_sensibly() {
    let mut citext = server("lower", "public", false, FunctionKind::Scalar);
    citext.source = "citext".into();
    let mut hidden = server("lower", "ext", false, FunctionKind::Scalar);
    hidden.qualified_only = true;
    let mut fuzzy = server("levenshtein", "public", false, FunctionKind::Scalar);
    fuzzy.source = "fuzzystrmatch".into();
    let functions = vec![citext, hidden, fuzzy, server("lower", "pg_catalog", true, FunctionKind::Scalar)];
    let catalog = FunctionCatalog::with_server(&DriverType::Postgres, FunctionList { functions, ..Default::default() });
    let lower: Vec<(u8, String)> =
        catalog.matches("lower", KindMask::EXPR, 10).into_iter().map(|m| (m.0, m.2.schema)).collect();
    assert_eq!(lower, [(TIER_USER, "ext".to_string()), (TIER_BUILTIN, String::new())]);
    let fuzzy = catalog.matches("leven", KindMask::EXPR, 10);
    assert_eq!(fuzzy.iter().map(|m| (m.0, m.2.source.as_str())).collect::<Vec<_>>(), [(TIER_BUILTIN, "fuzzystrmatch")]);
    assert_eq!(
        catalog.lookup(None, "lower").unwrap().summary,
        core::PACK.iter().find(|f| f.name == "lower").unwrap().summary
    );
}

// system.functions doesn't list combinator forms, so a connected server mustn't hide the ones the pack lists.
#[test]
fn clickhouse_combinator_forms_survive_the_server_list() {
    let functions =
        ["count", "sum", "toStartOfDay"].map(|name| server(name, "", true, FunctionKind::Aggregate)).to_vec();
    let catalog =
        FunctionCatalog::with_server(&DriverType::ClickHouse, FunctionList { functions, ..Default::default() });
    assert!(catalog.lookup(None, "countIf").is_some_and(|d| d.combinator.is_none()), "the pack's own entry");
    assert!(catalog.matches("countif", KindMask::EXPR, 10).iter().any(|m| m.2.name == "countIf"));
    assert!(catalog.lookup(None, "uniqIf").is_none(), "uniq isn't on this server");
}

#[test]
fn completion_matches_a_prefix_or_a_later_word() {
    assert_eq!(word_match("jsonb_build_object", "jsonb"), Some(0));
    assert_eq!(word_match("jsonb_build_object", "build"), Some(1));
    assert_eq!(word_match("toStartOfDay", "start"), Some(1));
    assert_eq!(word_match("COUNT_BIG", "big"), Some(1));
    assert_eq!(word_match("checksum_agg", "sum"), None, "not a word start");
    assert_eq!(word_match("abs", "absolute"), None);
    let catalog = FunctionCatalog::builtin(&DriverType::Postgres);
    let found = catalog.matches("build", KindMask::EXPR, 5);
    assert!(found.iter().all(|m| m.1 == 1) && found.iter().any(|m| m.2.name == "jsonb_build_object"), "{found:?}");
    assert_eq!(catalog.matches("j", KindMask::EXPR, 7).len(), 7, "the limit holds");
}
