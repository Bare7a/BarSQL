use std::ops::Range;
use std::sync::Arc;

use anyhow::Result;
use barsql_core::DriverType;
use barsql_sql::lang::quoting::{column_cache_key, format_sql_identifier};
use barsql_sql::lang::suggestions::{match_score, rank};
use barsql_sql::lang::{
    self, Catalog, ColumnMap, CompletionContext, HoverSubject, ItemKind, SqlDiagnostic, SqlLabels, TokenKind,
};
use gpui_kit::component::input::CompletionProvider;
use gpui_kit::component::{Rope, RopeExt};
use gpui_kit::{App, Task, Window};
use lsp_types::{
    CompletionContext as TriggerContext, CompletionItemKind, CompletionItemLabelDetails, CompletionResponse,
    CompletionTextEdit, InsertTextFormat, Range as LspRange, TextEdit,
};

use crate::i18n::I18n;
use crate::schema::{self, ColumnsLoad};

const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

#[derive(Clone)]
pub struct SqlLanguage {
    connection_id: String,
    driver: DriverType,
}

impl SqlLanguage {
    pub fn new(connection_id: String, driver: DriverType) -> Self {
        Self { connection_id, driver }
    }

    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    fn catalog(&self, cx: &App) -> Arc<Catalog> {
        schema::catalog(cx, &self.connection_id)
            .unwrap_or_else(|| Arc::new(Catalog::new(self.driver.clone(), Vec::new(), Vec::new())))
    }

    pub fn columns(&self, schema: &str, table: &str, cx: &mut App) -> ColumnsLoad {
        schema::columns(&self.connection_id, schema, table, cx)
    }

    // The span of the name at `offset`, and what it refers to.
    pub fn hover_subject(&self, text: &str, offset: usize, cx: &App) -> Option<(Range<usize>, HoverSubject)> {
        let offset = offset.min(text.len());
        let statements = lang::parse_statements(text, Some(&self.driver));
        let range = lang::current_statement_range(&statements, offset, text.len());
        let catalog = self.catalog(cx);
        let stmt = &text[range.clone()];
        let query = lang::analyze_hover(stmt, offset - range.start, &lang::parse_query(stmt, &catalog), &catalog)?;
        Some((range.start + query.start..range.start + query.end, query.subject))
    }
}

impl CompletionProvider for SqlLanguage {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: TriggerContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let sql = text.to_string();
        let offset = offset.min(sql.len());
        let statements = lang::parse_statements(&sql, Some(&self.driver));
        let range = lang::current_statement_range(&statements, offset, sql.len());
        // Quick suggestions stay quiet inside strings and comments.
        if in_string_or_comment(&sql[range.clone()], offset - range.start, &self.driver) {
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        }
        let catalog = self.catalog(cx);
        let parsed = lang::parse_query(&sql[range.clone()], &catalog);
        let loads: Vec<_> = lang::bindings_needing_columns(&sql[range.start..offset], &parsed, Some(&catalog))
            .into_iter()
            .map(|b| {
                (column_cache_key(&b.schema, &b.table), schema::columns(&self.connection_id, &b.schema, &b.table, cx))
            })
            .collect();
        let labels = cx.global::<I18n>().sql_labels();
        let (rope, driver) = (text.clone(), self.driver.clone());
        cx.spawn(async move |_| {
            let mut columns = ColumnMap::new();
            for (key, load) in loads {
                columns.insert(key, load.await.map(|c| c.as_ref().clone()).unwrap_or_default());
            }
            let ctx =
                CompletionContext { catalog: &catalog, columns_by_table: &columns, columns: &[], labels: &labels };
            let mut items = lang::build_completion_items(&ctx, &sql, offset, &parsed, range.start);
            items.sort_by(|a, b| a.sort_text.cmp(&b.sort_text).then_with(|| a.label.cmp(&b.label)));
            let before = &sql[range.start..offset];
            let replace =
                lang::completion_replace_range(offset, before, word_start(&sql, offset)..offset, Some(&driver));
            let edit = lsp_range(&rope, replace);
            Ok(CompletionResponse::Array(items.into_iter().map(|item| lsp_completion(item, edit)).collect()))
        })
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        let mut chars = new_text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => c.is_alphanumeric() || matches!(c, '_' | '.' | ' ' | ',' | '='),
            _ => false,
        }
    }
}

// Completion for a table view's WHERE filter. Columns rank above the WHERE keywords.
pub struct FilterCompletion {
    pub columns: Vec<(String, String)>,
    pub driver: DriverType,
}

const WHERE_KEYWORDS: [&str; 12] =
    ["AND", "OR", "NOT", "IN", "LIKE", "BETWEEN", "IS NULL", "IS NOT NULL", "EXISTS", "NULL", "TRUE", "FALSE"];

impl CompletionProvider for FilterCompletion {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: TriggerContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let condition = text.to_string();
        let offset = offset.min(condition.len());
        let start = word_start(&condition, offset);
        let prefix = condition[start..offset].to_lowercase();
        let range = lsp_range(text, start..offset);
        let labels = cx.global::<I18n>().sql_labels();
        let item =
            |label: &str, kind, detail: Option<String>, sort_text: String, insert: String| lsp_types::CompletionItem {
                label: label.to_string(),
                kind: Some(kind),
                detail,
                sort_text: Some(sort_text),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text: insert })),
                ..Default::default()
            };
        let mut items: Vec<lsp_types::CompletionItem> = self
            .columns
            .iter()
            .filter_map(|(name, data_type)| {
                let score = match_score(name, &prefix)?;
                let detail = if data_type.is_empty() { labels.column.clone() } else { data_type.clone() };
                let insert = format_sql_identifier(name, &self.driver);
                Some(item(name, CompletionItemKind::FIELD, Some(detail), rank(0, score, name), insert))
            })
            .collect();
        items.extend(WHERE_KEYWORDS.iter().filter_map(|keyword| {
            let score = match_score(keyword, &prefix)?;
            Some(item(keyword, CompletionItemKind::KEYWORD, None, rank(1, score, keyword), keyword.to_string()))
        }));
        items.sort_by(|a, b| a.sort_text.cmp(&b.sort_text));
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        let mut chars = new_text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => c.is_alphanumeric() || matches!(c, '_' | '.' | ' ' | '=' | '<' | '>' | '!'),
            _ => false,
        }
    }
}

// Unknown-table warnings, minus the reference under the caret so typing doesn't flicker.
pub fn diagnostics(sql: &str, catalog: &Catalog, labels: &SqlLabels, cursor: usize) -> Vec<SqlDiagnostic> {
    lang::collect_schema_diagnostics(sql, catalog, labels)
        .into_iter()
        .filter(|d| cursor < d.start || cursor > d.end)
        .collect()
}

pub fn lsp_range(rope: &Rope, range: Range<usize>) -> LspRange {
    LspRange::new(rope.offset_to_position(range.start), rope.offset_to_position(range.end))
}

fn lsp_completion(item: lang::CompletionItem, range: LspRange) -> lsp_types::CompletionItem {
    let kind = match item.kind {
        ItemKind::Keyword => CompletionItemKind::KEYWORD,
        ItemKind::Field => CompletionItemKind::FIELD,
        ItemKind::Class => CompletionItemKind::CLASS,
        ItemKind::Module => CompletionItemKind::MODULE,
        ItemKind::Function => CompletionItemKind::FUNCTION,
    };
    lsp_types::CompletionItem {
        kind: Some(kind),
        detail: item.detail,
        label_details: item
            .label_detail
            .map(|detail| CompletionItemLabelDetails { detail: Some(detail), description: None }),
        sort_text: Some(item.sort_text),
        insert_text_format: item.snippet.then_some(InsertTextFormat::SNIPPET),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text: item.insert_text })),
        label: item.label,
        ..Default::default()
    }
}

pub fn word_start(text: &str, offset: usize) -> usize {
    text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| !c.is_whitespace() && !WORD_SEPARATORS.contains(*c))
        .last()
        .map_or(offset, |(i, _)| i)
}

// A line comment runs to the end of its line, so a caret at that end is still inside it.
fn in_string_or_comment(stmt: &str, offset: usize, driver: &DriverType) -> bool {
    lang::tokenize(stmt, Some(driver)).iter().any(|t| {
        let line_comment = t.kind == TokenKind::Comment && !t.text.starts_with("/*");
        matches!(t.kind, TokenKind::String | TokenKind::Comment)
            && t.start < offset
            && (offset < t.end || (offset == t.end && (t.unterminated || line_comment)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_end_at_separators() {
        assert_eq!(word_start("SELECT us", 9), 7);
        assert_eq!(word_start("SELECT u.na", 11), 9);
        assert_eq!(word_start("SELECT naïve", 13), 7);
        assert_eq!(word_start("SELECT ", 7), 7);
    }

    #[test]
    fn strings_and_comments_are_detected_at_the_caret() {
        let pg = DriverType::Postgres;
        assert!(in_string_or_comment("SELECT 'ab", 9, &pg));
        assert!(!in_string_or_comment("SELECT 'ab'", 11, &pg));
        assert!(in_string_or_comment("SELECT 1 -- note", 16, &pg));
        assert!(!in_string_or_comment("SELECT 1 /* x */", 16, &pg));
        assert!(!in_string_or_comment("SELECT us", 9, &pg));
    }
}
