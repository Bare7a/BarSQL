// Times the SQL editor's per-keystroke work on a fixed catalog and prints the results as JSON.
// Run `cargo bench -p barsql-sql --bench lang_pipeline`. Set BENCH_OUT to also write the JSON to a file.
use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use std::sync::Arc;

use barsql_core::{
    ColumnInfo, DriverType, FunctionInfo, FunctionKind, FunctionList, FunctionSignature, SchemaInfo, TableInfo,
};
use barsql_sql::lang::quoting::column_cache_key;
use barsql_sql::lang::{
    Catalog, ColumnMap, CompletionContext, FunctionCatalog, SqlLabels, analyze_hover, bindings_needing_columns,
    build_completion_items, collect_schema_diagnostics, completion_replace_range, current_statement_range, parse_query,
    parse_statements,
};
use serde_json::{Value, json};

const PG: DriverType = DriverType::Postgres;
const TYPES: [&str; 8] = ["integer", "text", "numeric(12,2)", "timestamptz", "boolean", "uuid", "jsonb", "bigint"];

fn column(name: &str, data_type: &str, nullable: bool, primary: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: data_type.into(),
        is_nullable: nullable,
        is_primary: primary,
        ..Default::default()
    }
}

fn foreign(name: &str, table: &str) -> ColumnInfo {
    ColumnInfo {
        is_foreign: true,
        foreign_table: table.into(),
        foreign_column: "id".into(),
        ..column(name, "integer", false, false)
    }
}

fn make_columns(table: &str) -> Vec<ColumnInfo> {
    let mut cols = vec![
        column("id", "integer", false, true),
        column("created_at", "timestamptz", false, false),
        column("updated_at", "timestamptz", true, false),
    ];
    for i in 0..37 {
        let mut c = column(&format!("{table}_c{i}"), TYPES[i % TYPES.len()], i % 3 != 0, false);
        if i == 5 {
            c = ColumnInfo { is_foreign: true, foreign_table: "users".into(), foreign_column: "id".into(), ..c };
        }
        cols.push(c);
    }
    cols
}

struct Fixture {
    catalog: Catalog,
    columns: ColumnMap,
    labels: SqlLabels,
}

// Like a Postgres server's list: built-ins with overloads, then an extension's and the app's own functions.
fn server_functions() -> FunctionList {
    let words = ["json", "array", "text", "date", "time", "range", "agg", "path", "build", "object", "set", "to"];
    let functions = (0..3000)
        .map(|i| {
            let name = format!("{}_{}_{i}", words[i % words.len()], words[(i / 7) % words.len()]);
            let (schema, builtin, source) = match i % 10 {
                0..=7 => ("pg_catalog", true, ""),
                8 => ("public", false, "postgis"),
                _ => ("app", false, ""),
            };
            let signature = FunctionSignature { args: "value anyelement, path text[]".into(), returns: "jsonb".into() };
            FunctionInfo {
                name,
                schema: schema.into(),
                kind: if i % 13 == 0 { FunctionKind::Aggregate } else { FunctionKind::Scalar },
                signatures: vec![signature; 1 + i % 3],
                description: "returns a value".into(),
                builtin,
                qualified_only: schema == "app",
                source: source.into(),
                ..Default::default()
            }
        })
        .collect();
    FunctionList { functions, ..Default::default() }
}

fn fixture() -> Fixture {
    let named = ["users", "orders", "order_items", "products", "customers"];
    let mut tables: Vec<TableInfo> = named
        .iter()
        .map(|n| TableInfo { schema: "public".into(), name: n.to_string(), kind: "table".into() })
        .collect();
    for i in named.len()..250 {
        let kind = if i % 10 == 0 { "view" } else { "table" };
        tables.push(TableInfo { schema: "public".into(), name: format!("t_{i:03}"), kind: kind.into() });
    }
    let mut columns: ColumnMap =
        tables.iter().map(|t| (column_cache_key(&t.schema, &t.name), make_columns(&t.name))).collect();
    let extend = |columns: &mut ColumnMap, table: &str, extra: Vec<ColumnInfo>| {
        columns.get_mut(&format!("public.{table}")).expect("table").extend(extra);
    };
    extend(
        &mut columns,
        "users",
        vec![column("email", "text", false, false), column("full_name", "text", true, false)],
    );
    extend(
        &mut columns,
        "orders",
        vec![
            column("total", "numeric(12,2)", false, false),
            foreign("user_id", "users"),
            foreign("customer_id", "customers"),
        ],
    );
    extend(&mut columns, "order_items", vec![foreign("order_id", "orders"), foreign("product_id", "products")]);
    let schemas = ["public", "analytics", "audit"].iter().map(|s| SchemaInfo { name: s.to_string() }).collect();
    let functions = Arc::new(FunctionCatalog::with_server(&PG, server_functions()));
    let catalog = Catalog::new(PG, schemas, tables).with_functions(functions);
    Fixture { catalog, columns, labels: SqlLabels::default() }
}

impl Fixture {
    fn glyph_pass(&self, text: &str) -> usize {
        parse_statements(text, Some(&PG)).len()
    }

    fn completion_request(&self, text: &str, offset: usize) -> usize {
        let range = current_statement_range(&parse_statements(text, Some(&PG)), offset, text.len());
        let before = &text[range.start..offset];
        let parsed = parse_query(&text[range.clone()], &self.catalog);
        black_box(bindings_needing_columns(before, &parsed, Some(&self.catalog)));
        let ctx = CompletionContext {
            catalog: &self.catalog,
            columns_by_table: &self.columns,
            columns: &[],
            labels: &self.labels,
        };
        let items = build_completion_items(&ctx, text, offset, &parsed, range.start);
        black_box(completion_replace_range(offset, before, offset..offset, Some(&PG)));
        items.len()
    }

    fn diagnostics_fire(&self, text: &str) -> usize {
        collect_schema_diagnostics(text, &self.catalog, &self.labels).len()
    }

    fn hover_request(&self, text: &str, offset: usize) -> bool {
        let range = current_statement_range(&parse_statements(text, Some(&PG)), offset, text.len());
        let stmt = &text[range.clone()];
        let parsed = parse_query(stmt, &self.catalog);
        analyze_hover(stmt, offset - range.start, &parsed, &self.catalog).is_some()
    }
}

#[derive(Default)]
struct Buckets(BTreeMap<&'static str, Vec<f64>>);

impl Buckets {
    fn time<T>(&mut self, bucket: &'static str, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let out = black_box(work());
        self.0.entry(bucket).or_default().push(started.elapsed().as_secs_f64() * 1000.0);
        out
    }
}

fn stats(samples: &[f64]) -> Value {
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    let total: f64 = s.iter().sum();
    let q = |p: f64| s.get(((p * s.len() as f64) as usize).min(s.len().saturating_sub(1))).copied().unwrap_or(0.0);
    json!({
        "n": s.len(),
        "meanMs": total / s.len().max(1) as f64,
        "p50Ms": q(0.5),
        "p95Ms": q(0.95),
        "maxMs": s.last().copied().unwrap_or(0.0),
        "totalMs": total,
    })
}

#[cfg(unix)]
fn usage() -> (f64, f64, f64) {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the struct it is handed.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let ms = |t: libc::timeval| t.tv_sec as f64 * 1000.0 + t.tv_usec as f64 / 1000.0;
    let max_rss_mb =
        if cfg!(target_os = "macos") { ru.ru_maxrss as f64 / 1_048_576.0 } else { ru.ru_maxrss as f64 / 1024.0 };
    (ms(ru.ru_utime), ms(ru.ru_stime), max_rss_mb)
}

#[cfg(not(unix))]
fn usage() -> (f64, f64, f64) {
    (0.0, 0.0, 0.0)
}

fn run_workload(work: impl FnOnce(&mut Buckets) -> usize) -> Value {
    let mut buckets = Buckets::default();
    let (user0, sys0, _) = usage();
    let started = Instant::now();
    let checksum = work(&mut buckets);
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (user1, sys1, _) = usage();
    let per_bucket: serde_json::Map<String, Value> = buckets.0.iter().map(|(k, v)| (k.to_string(), stats(v))).collect();
    json!({ "buckets": per_bucket, "wallMs": wall_ms, "cpuUserMs": user1 - user0, "cpuSysMs": sys1 - sys0, "checksum": checksum })
}

fn typed_query(salt: &str) -> String {
    format!(
        "SELECT u.id, u.email, o.total FROM users u JOIN orders o ON o.user_id = u.id \
         WHERE u.email LIKE 'a{salt}%' AND o.total > 100 ORDER BY o.created_at DESC"
    )
}

// Replays typing `typed` after `prefix`, one keystroke at a time.
fn typing_session(f: &Fixture, prefix: &str, typed: &str, b: &mut Buckets, diag_every: usize) -> usize {
    let mut checksum = 0;
    for k in 1..=typed.len() {
        let text = format!("{prefix}{}", &typed[..k]);
        checksum += b.time("glyphs", || f.glyph_pass(&text));
        checksum += b.time("completion", || f.completion_request(&text, text.len()));
        if k % diag_every == 0 || k == typed.len() {
            checksum += b.time("diagnostics", || f.diagnostics_fire(&text));
        }
    }
    checksum
}

fn big_file_prefix(salt: &str) -> String {
    let lines: Vec<String> = (0..150)
        .map(|i| {
            let t = format!("t_{:03}", (i % 245) + 5);
            format!("SELECT id, created_at, {t}_c{} FROM {t} WHERE {t}_c3 = {i} /*{salt}*/;", i % 37)
        })
        .collect();
    format!("{}\n", lines.join("\n"))
}

fn w1(f: &Fixture, b: &mut Buckets) -> usize {
    (0..8).map(|s| typing_session(f, "", &typed_query(&format!("s{s}")), b, 12)).sum()
}

fn w2(f: &Fixture, b: &mut Buckets) -> usize {
    (0..6).map(|s| typing_session(f, &big_file_prefix(&format!("s{s}")), &typed_query(&format!("s{s}")), b, 12)).sum()
}

fn w3(f: &Fixture, b: &mut Buckets) -> usize {
    let mut checksum = 0;
    for s in 0..8 {
        let fixed = format!(
            "/*s{s}*/ SELECT * FROM users u JOIN orders o ON o.user_id = u.id \
             JOIN order_items oi ON oi.order_id = o.id JOIN products p ON p.id = oi.product_id \
             JOIN customers c ON c.id = o.customer_id WHERE "
        );
        let tail = "created_at > now() AND u.email = c.customers_c1";
        checksum += typing_session(f, "", &fixed, b, 1_000_000);
        for k in 1..=tail.len() {
            let text = format!("{fixed}{}", &tail[..k]);
            checksum += b.time("completion-wide", || f.completion_request(&text, text.len()));
        }
    }
    checksum
}

fn w4(f: &Fixture, b: &mut Buckets) -> usize {
    let mut checksum = 0;
    for s in 0..6 {
        let text = typed_query(&format!("s{s}"));
        for _ in 0..40 {
            for offset in (0..text.len()).step_by(3) {
                checksum += usize::from(b.time("hover", || f.hover_request(&text, offset)));
            }
        }
    }
    checksum
}

// Calls typed into the select list and WHERE, where every keystroke searches the functions.
fn w5(f: &Fixture, b: &mut Buckets) -> usize {
    let typed = "SELECT jsonb_build_object('id', u.id), count(*), json_agg(o.total) FROM users u JOIN orders o \
                 ON o.user_id = u.id WHERE lower(u.email) = to_char(now(), 'YYYY') AND date_trunc('day', o.created_at)";
    (0..6).map(|s| typing_session(f, &format!("/*s{s}*/ "), typed, b, 1_000_000)).sum()
}

fn main() {
    // Only `cargo bench` passes --bench. `cargo test --benches` and test listing get a no-op.
    if !std::env::args().any(|a| a == "--bench") {
        return;
    }
    let f = fixture();
    {
        let mut warm = Buckets::default();
        typing_session(&f, "", &typed_query("warm"), &mut warm, 10);
        typing_session(&f, &big_file_prefix("warm"), &typed_query("warm"), &mut warm, 10);
    }
    let workloads = json!({
        "typing_small": run_workload(|b| w1(&f, b)),
        "typing_bigfile": run_workload(|b| w2(&f, b)),
        "completion_wide": run_workload(|b| w3(&f, b)),
        "hover_sweep": run_workload(|b| w4(&f, b)),
        "typing_functions": run_workload(|b| w5(&f, b)),
    });
    let (_, _, max_rss_mb) = usage();
    let out = json!({
        "side": std::env::var("BENCH_SIDE").unwrap_or_else(|_| "rust".into()),
        "workloads": workloads,
        "memory": { "maxRssMB": max_rss_mb },
    });
    let text = serde_json::to_string_pretty(&out).expect("json");
    if let Ok(dest) = std::env::var("BENCH_OUT") {
        std::fs::write(dest, &text).expect("write BENCH_OUT");
    }
    println!("{text}");
}
