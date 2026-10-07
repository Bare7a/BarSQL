use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

use barsql_core::{DriverType, FunctionKind};

use super::catalog::{Catalog, TableBinding};
use super::context::{Clause, CursorSlot, SqlCursor, StatementShape, analyze_cursor};
use super::functions::{CallForm, FunctionDoc, KindMask};
use super::query::{ParsedQuery, QueryTableRef, resolve_dot_completion};
use super::quoting::{column_cache_key, format_sql_identifier, unquote_ident};
use super::suggestions::{
    CompletionContext, CompletionItem, ItemKind, keyword_item, keywords_for_shape, rank, suggest_columns_for_table,
    suggest_columns_in_scope, suggest_cte_items, suggest_query_table_refs, suggest_schemas, suggest_tables,
    suggest_value_items, suggest_virtual_columns, with_leading_space,
};

const MAX_ITEMS: usize = 100;
// Functions only show once a letter is typed, so this caps a short prefix's flood.
const MAX_FUNCTIONS: usize = 60;

// What the caret is followed by and what was typed, which shape a function's inserted text.
struct Typing<'a> {
    // The partial word as typed, not lowercased.
    word: &'a str,
    // A `(` already follows, so only the name goes in.
    paren_follows: bool,
}

// Which functions fit where: aggregates and window functions only where the query has groups or rows to work
// on, table functions only in FROM.
fn function_kinds(clause: Clause) -> KindMask {
    match clause {
        Clause::Select | Clause::OrderBy => KindMask::EXPR,
        Clause::Having => KindMask::SCALAR.with(KindMask::AGGREGATE),
        Clause::Where
        | Clause::On
        | Clause::Set
        | Clause::Values
        | Clause::GroupBy
        | Clause::Returning
        | Clause::From
        | Clause::Other => KindMask::SCALAR,
        Clause::Target | Clause::Limit => KindMask::NONE,
    }
}

// `$`, `\` and `}` are snippet syntax.
fn snippet_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '$' | '\\' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// The kind and the first overload's return type, then the schema of a user function.
fn function_detail(ctx: &CompletionContext, doc: &FunctionDoc) -> String {
    let labels = ctx.labels;
    let kind = match doc.kind {
        FunctionKind::Scalar if doc.builtin => &labels.function,
        FunctionKind::Scalar => &labels.user_function,
        FunctionKind::Aggregate => &labels.aggregate_function,
        FunctionKind::Window => &labels.window_function,
        FunctionKind::Table => &labels.table_function,
    };
    let mut parts = vec![kind.clone()];
    if let Some((_, returns)) = doc.signatures.first().filter(|(_, r)| !r.is_empty()) {
        parts.push(returns.clone());
    }
    if !doc.schema.is_empty() {
        parts.push(doc.schema.clone());
    }
    parts.join(" · ")
}

fn function_item(
    ctx: &CompletionContext,
    (tier, score, doc): (u8, u8, FunctionDoc),
    typing: &Typing,
) -> CompletionItem {
    let driver = ctx.driver();
    let name = if doc.builtin {
        // Typing in capitals asks for capitals, where the server ignores the case anyway.
        let shout = typing.word.chars().any(|c| c.is_ascii_alphabetic())
            && !typing.word.chars().any(|c| c.is_ascii_lowercase())
            && !ctx.catalog.functions.case_sensitive(&doc.name);
        if shout { doc.name.to_ascii_uppercase() } else { doc.name.clone() }
    } else {
        let name = format_sql_identifier(&doc.name, driver);
        if doc.qualified_only { format!("{}.{name}", format_sql_identifier(&doc.schema, driver)) } else { name }
    };
    let (insert_text, snippet) = match doc.call {
        _ if typing.paren_follows => (name, false),
        CallForm::NoParens => (name, false),
        CallForm::Empty => (format!("{name}()"), false),
        CallForm::Args => (format!("{}($0)", snippet_escape(&name)), true),
    };
    let label_detail = match doc.call {
        CallForm::NoParens => None,
        _ => doc.signatures.first().map(|(args, _)| format!("({args})")),
    };
    CompletionItem {
        sort_text: rank(tier, score, &doc.name),
        detail: Some(function_detail(ctx, &doc)),
        label: doc.name,
        kind: ItemKind::Function,
        label_detail,
        insert_text,
        snippet,
        filter_text: None,
    }
}

// Nothing until a letter is typed, so an empty prefix doesn't list a thousand built-ins. Quoted words are
// identifiers, not calls.
fn function_items(ctx: &CompletionContext, prefix: &str, kinds: KindMask, typing: &Typing) -> Vec<CompletionItem> {
    if prefix.is_empty() || kinds.is_empty() || typing.word.starts_with(['"', '`', '[', '\'']) {
        return Vec::new();
    }
    let matches = ctx.catalog.functions.matches(prefix, kinds, MAX_FUNCTIONS);
    matches.into_iter().map(|m| function_item(ctx, m, typing)).collect()
}

// Lets the editor load just the columns this completion can show. Without a catalog, a dot qualifier
// only resolves through the query's own bindings.
pub fn bindings_needing_columns(before: &str, parsed: &ParsedQuery, catalog: Option<&Catalog>) -> Vec<TableBinding> {
    let cursor = analyze_cursor(before, catalog.map(|c| &c.driver));
    let mut needed = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |b: Option<TableBinding>| {
        if let Some(b) = b
            && seen.insert(column_cache_key(&b.schema, &b.table))
        {
            needed.push(b);
        }
    };
    // CTEs and derived-table aliases have no schema entry to load columns from.
    let is_virtual = |name: &str| parsed.virtual_columns.contains_key(&name.to_lowercase());
    let query_tables = || {
        parsed
            .query_tables
            .iter()
            .filter(|r| !is_virtual(&r.table))
            .map(|r| TableBinding { schema: r.schema.clone(), table: r.table.clone() })
            .collect::<Vec<_>>()
    };
    let bindings = || parsed.bindings.values().filter(|b| !is_virtual(&b.table)).cloned().collect::<Vec<_>>();

    match &cursor.slot {
        CursorSlot::None | CursorSlot::Table { .. } | CursorSlot::Limit { .. } => {}
        CursorSlot::Dot { segments, .. } => {
            if !is_virtual(&unquote_ident(&segments[0])) {
                match catalog {
                    // Full resolution handles schema.table.column and tables outside the FROM clause.
                    Some(catalog) => add(resolve_dot_completion(segments, parsed, catalog)),
                    None => add(parsed.bindings.get(&unquote_ident(&segments[0]).to_lowercase()).cloned()),
                }
            }
        }
        CursorSlot::Value { .. } => {
            let from_bindings = bindings();
            let empty = from_bindings.is_empty();
            from_bindings.into_iter().for_each(|b| add(Some(b)));
            if empty {
                query_tables().into_iter().for_each(|b| add(Some(b)));
            }
        }
        CursorSlot::FilterStart
        | CursorSlot::SetColumn { .. }
        | CursorSlot::InsertColumns { .. }
        | CursorSlot::OrderGroup { .. } => {
            query_tables().into_iter().for_each(|b| add(Some(b)));
        }
        CursorSlot::General { in_filter, .. } => {
            let list = if *in_filter { query_tables() } else { bindings() };
            list.into_iter().for_each(|b| add(Some(b)));
        }
    }
    needed
}

// Suggests `a.fk = b.pk` after ON from the joined tables' foreign keys, in both directions.
fn fk_join_items(ctx: &CompletionContext, query_tables: &[QueryTableRef]) -> Vec<CompletionItem> {
    let refs: Vec<&QueryTableRef> = query_tables
        .iter()
        .enumerate()
        .filter(|(i, r)| query_tables.iter().position(|x| x.schema == r.schema && x.table == r.table) == Some(*i))
        .map(|(_, r)| r)
        .collect();
    let qualify = |r: &QueryTableRef, column: &str| {
        format!(
            "{}.{}",
            format_sql_identifier(r.alias.as_deref().unwrap_or(&r.table), ctx.driver()),
            format_sql_identifier(column, ctx.driver())
        )
    };
    let mut items = Vec::new();
    for (fi, from) in refs.iter().enumerate() {
        let Some(cols) = ctx.columns_by_table.get(&column_cache_key(&from.schema, &from.table)) else { continue };
        for col in cols {
            if col.foreign_table.is_empty() || col.foreign_column.is_empty() {
                continue;
            }
            let target = refs
                .iter()
                .enumerate()
                .find(|(ti, r)| *ti != fi && r.table.to_lowercase() == col.foreign_table.to_lowercase())
                .map(|(_, r)| *r);
            let Some(target) = target else { continue };
            let expr = format!("{} = {}", qualify(from, &col.name), qualify(target, &col.foreign_column));
            items.push(CompletionItem {
                sort_text: rank(0, 0, &expr),
                label: expr.clone(),
                kind: ItemKind::Field,
                detail: Some(ctx.labels.foreign_key.clone()),
                insert_text: expr,
                filter_text: None,
                label_detail: None,
                snippet: false,
            });
        }
    }
    items
}

fn virtual_detail(ctx: &CompletionContext, parsed: &ParsedQuery, name_lc: &str) -> String {
    if parsed.is_cte(name_lc) { ctx.labels.cte_column.clone() } else { ctx.labels.subquery_column.clone() }
}

// Derived-table aliases are always in scope. CTEs only once FROM or JOIN references them.
fn in_scope_virtual_column_items(
    ctx: &CompletionContext,
    parsed: &ParsedQuery,
    lc_prefix: &str,
) -> Vec<CompletionItem> {
    let referenced: HashSet<String> = parsed.query_tables.iter().map(|t| t.table.to_lowercase()).collect();
    let mut items = Vec::new();
    for (name, cols) in parsed.virtual_columns.iter() {
        if parsed.is_cte(name) && !referenced.contains(name) {
            continue;
        }
        items.extend(suggest_virtual_columns(cols, lc_prefix, ctx.driver(), &virtual_detail(ctx, parsed, name)));
    }
    items
}

fn dot_items(ctx: &CompletionContext, segments: &[String], prefix: &str, parsed: &ParsedQuery) -> Vec<CompletionItem> {
    let prefix = prefix.to_lowercase();
    if segments.len() == 1 {
        let name_lc = unquote_ident(&segments[0]).to_lowercase();
        if let Some(cols) = parsed.virtual_columns.get(&name_lc) {
            return suggest_virtual_columns(cols, &prefix, ctx.driver(), &virtual_detail(ctx, parsed, &name_lc));
        }
    }
    if let Some(binding) = resolve_dot_completion(segments, parsed, ctx.catalog) {
        return suggest_columns_for_table(ctx, &binding, &prefix);
    }
    if segments.len() == 1 {
        let schema = unquote_ident(&segments[0]);
        if !ctx.catalog.tables_in_schema(&schema).is_empty() {
            return suggest_tables(ctx, &prefix, Some(&schema));
        }
    }
    Vec::new()
}

fn table_slot_items(
    ctx: &CompletionContext,
    prefix: &str,
    ctes: &[String],
    shape: &StatementShape,
    typing: &Typing,
) -> Vec<CompletionItem> {
    // CTEs go first at tier 0 so they survive the item cap on large schemas.
    let mut items = suggest_cte_items(ctes, prefix, ctx);
    items.extend(suggest_tables(ctx, prefix, None));
    items.extend(suggest_schemas(ctx, prefix));
    if shape.clause == Clause::From {
        items.extend(function_items(ctx, prefix, KindMask::TABLE, typing));
    }
    items
}

fn keyword_items<'k>(
    keywords: impl IntoIterator<Item = &'k str>,
    prefix: &str,
) -> impl Iterator<Item = CompletionItem> {
    keywords.into_iter().filter_map(move |kw| keyword_item(kw, prefix))
}

fn general_items(
    ctx: &CompletionContext,
    prefix: &str,
    in_filter: bool,
    expects_expr: bool,
    shape: &StatementShape,
    parsed: &ParsedQuery,
    typing: &Typing,
) -> Vec<CompletionItem> {
    let query_tables = &parsed.query_tables;
    let mut items = Vec::new();
    // Identifiers only when the previous token expects one. Keywords always pass through.
    if in_filter && expects_expr {
        if shape.after_on_keyword && prefix.is_empty() {
            items.extend(fk_join_items(ctx, query_tables));
        }
        items.extend(suggest_query_table_refs(ctx, query_tables, prefix));
        items.extend(suggest_columns_in_scope(ctx, query_tables, prefix, None));
        items.extend(in_scope_virtual_column_items(ctx, parsed, prefix));
    }
    items.extend(keyword_items(keywords_for_shape(shape, Some(ctx.driver())), prefix));
    if expects_expr && shape.in_select_list {
        // Table refs too, so SELECT-list edits can qualify columns.
        items.extend(suggest_query_table_refs(ctx, query_tables, prefix));
        items.extend(suggest_columns_in_scope(ctx, query_tables, prefix, None));
    }
    if expects_expr && !shape.has_from && !in_filter {
        items.extend(suggest_schemas(ctx, prefix));
    }
    if expects_expr && !shape.at_statement_start {
        items.extend(function_items(ctx, prefix, function_kinds(shape.clause), typing));
    }
    items
}

fn completion_items(
    ctx: &CompletionContext,
    parsed: &ParsedQuery,
    cursor: &SqlCursor,
    typing: &Typing,
) -> Vec<CompletionItem> {
    let query_tables = &parsed.query_tables;
    let shape = &cursor.shape;
    let spaced =
        |items: Vec<CompletionItem>, leading_space: bool| if leading_space { with_leading_space(items) } else { items };
    match &cursor.slot {
        CursorSlot::None => Vec::new(),
        CursorSlot::Dot { segments, prefix, .. } => dot_items(ctx, segments, prefix, parsed),
        CursorSlot::Table { prefix, leading_space, .. } => {
            spaced(table_slot_items(ctx, prefix, &parsed.ctes, shape, typing), *leading_space)
        }
        CursorSlot::InsertColumns { used, prefix, .. } => {
            let used: HashSet<&str> = used.iter().map(String::as_str).collect();
            suggest_columns_in_scope(ctx, query_tables, prefix, None)
                .into_iter()
                .filter(|item| !used.contains(item.label.to_lowercase().as_str()))
                .collect()
        }
        CursorSlot::SetColumn { prefix, leading_space, .. } => {
            spaced(suggest_columns_in_scope(ctx, query_tables, prefix, None), *leading_space)
        }
        CursorSlot::FilterStart => {
            let mut items = if shape.after_on_keyword { fk_join_items(ctx, query_tables) } else { Vec::new() };
            items.extend(suggest_query_table_refs(ctx, query_tables, ""));
            items.extend(suggest_columns_in_scope(ctx, query_tables, "", None));
            items.extend(in_scope_virtual_column_items(ctx, parsed, ""));
            with_leading_space(items)
        }
        CursorSlot::Value { prefix, .. } => {
            let mut items = suggest_value_items(ctx, query_tables, prefix);
            items.extend(in_scope_virtual_column_items(ctx, parsed, prefix));
            items.extend(function_items(ctx, prefix, function_kinds(shape.clause), typing));
            items
        }
        CursorSlot::OrderGroup {
            expects_expr, direction_allowed, trailing_keywords, prefix, leading_space, ..
        } => {
            // Columns and tables only at the start of a sort term, not after a finished one.
            let mut items = Vec::new();
            if *expects_expr {
                items.extend(suggest_query_table_refs(ctx, query_tables, prefix));
                items.extend(suggest_columns_in_scope(ctx, query_tables, prefix, None));
            }
            let directions: &[&str] = if *direction_allowed { &["ASC", "DESC"] } else { &[] };
            items.extend(keyword_items(directions.iter().copied().chain(trailing_keywords.iter().copied()), prefix));
            if *expects_expr {
                items.extend(in_scope_virtual_column_items(ctx, parsed, prefix));
                items.extend(function_items(ctx, prefix, function_kinds(shape.clause), typing));
            }
            spaced(items, *leading_space)
        }
        CursorSlot::Limit { offer_offset, prefix, .. } => {
            if *offer_offset {
                keyword_item("OFFSET", prefix).into_iter().collect()
            } else {
                Vec::new()
            }
        }
        CursorSlot::General { prefix, in_filter, expects_expr, .. } => {
            general_items(ctx, prefix, *in_filter, *expects_expr, shape, parsed, typing)
        }
    }
}

// `statement_start` limits clause context to the current statement.
pub fn build_completion_items(
    ctx: &CompletionContext,
    text: &str,
    position: usize,
    parsed: &ParsedQuery,
    statement_start: usize,
) -> Vec<CompletionItem> {
    let before = &text[statement_start.min(position)..position];
    let cursor = analyze_cursor(before, Some(ctx.driver()));
    let typing = Typing {
        word: &before[before.len() - cursor.slot.replace_len().min(before.len())..],
        paren_follows: text[position..].starts_with('('),
    };
    let items = completion_items(ctx, parsed, &cursor, &typing);
    if items.len() <= MAX_ITEMS {
        return items;
    }
    // sort_text starts with tier and score, so bucketing on those two characters finds the top 100
    // without a full sort. The editor re-sorts what's left.
    let mut buckets: BTreeMap<String, Vec<CompletionItem>> = BTreeMap::new();
    for item in items {
        let key: String = item.sort_text.chars().take(2).collect();
        buckets.entry(key).or_default().push(item);
    }
    buckets.into_values().flatten().take(MAX_ITEMS).collect()
}

// `fallback` is the editor's word range at the caret. Clause-start slots insert at the caret, and a
// partial word gets replaced whole.
pub fn completion_replace_range(
    offset: usize,
    text_before: &str,
    fallback: Range<usize>,
    driver: Option<&DriverType>,
) -> Range<usize> {
    let slot = analyze_cursor(text_before, driver).slot;
    if slot.leading_space() {
        return offset..offset;
    }
    if slot == CursorSlot::None {
        return fallback;
    }
    let replace_len = slot.replace_len();
    if replace_len > 0 {
        let line_start = offset - (text_before.len() - text_before.rfind('\n').map_or(0, |p| p + 1));
        return offset.saturating_sub(replace_len).max(line_start)..offset;
    }
    // With no partial word, General uses the editor's word range and structured slots insert at the caret.
    if matches!(slot, CursorSlot::General { .. }) { fallback } else { offset..offset }
}
