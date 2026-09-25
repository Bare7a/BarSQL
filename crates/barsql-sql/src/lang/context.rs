use barsql_core::DriverType;

use super::tokens::{Token, TokenKind, is_ident_like, is_keyword, is_punct, tokenize};

// Computed from the text before the cursor only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatementShape {
    pub at_statement_start: bool,
    pub has_from: bool,
    pub has_join: bool,
    pub in_where: bool,
    // WHERE, HAVING or ON seen.
    pub in_filter_clause: bool,
    // SELECT present, FROM not yet.
    pub in_select_list: bool,
    // INSERT or REPLACE.
    pub has_insert: bool,
    pub has_update: bool,
    pub has_case: bool,
    pub order_by_seen: bool,
    pub group_by_seen: bool,
    // INSERT followed by VALUES or SELECT.
    pub insert_body: bool,
    // A write statement far enough along for RETURNING.
    pub returning_slot: bool,
    // FROM seen and not inside WHERE, so a JOIN would parse here.
    pub joinable: bool,
    // Caret is right after a JOIN's ON.
    pub after_on_keyword: bool,
}

// `prefix` is the partial word, lowercased. `replace_len` is how many bytes it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorSlot {
    // Inside a comment or a plain string literal.
    None,
    // After `alias.` or `schema.table.`. Segments and prefix stay raw for the resolvers to unquote.
    Dot {
        segments: Vec<String>,
        prefix: String,
        replace_len: usize,
    },
    Table {
        prefix: String,
        replace_len: usize,
        leading_space: bool,
    },
    InsertColumns {
        used: Vec<String>,
        prefix: String,
        replace_len: usize,
    },
    SetColumn {
        prefix: String,
        replace_len: usize,
        leading_space: bool,
    },
    // Caret right after WHERE, AND, etc. Columns follow with a leading space.
    FilterStart,
    Value {
        prefix: String,
        replace_len: usize,
    },
    OrderGroup {
        group: bool,
        expects_expr: bool,
        direction_allowed: bool,
        trailing_keywords: Vec<&'static str>,
        prefix: String,
        replace_len: usize,
        leading_space: bool,
    },
    Limit {
        offer_offset: bool,
        prefix: String,
        replace_len: usize,
    },
    General {
        prefix: String,
        replace_len: usize,
        in_filter: bool,
        expects_expr: bool,
    },
}

impl CursorSlot {
    // Clause-start slots insert at the caret with a leading space.
    pub fn leading_space(&self) -> bool {
        match self {
            Self::Table { leading_space, .. }
            | Self::SetColumn { leading_space, .. }
            | Self::OrderGroup { leading_space, .. } => *leading_space,
            Self::FilterStart => true,
            _ => false,
        }
    }

    pub fn replace_len(&self) -> usize {
        match self {
            Self::Dot { replace_len, .. }
            | Self::Table { replace_len, .. }
            | Self::InsertColumns { replace_len, .. }
            | Self::SetColumn { replace_len, .. }
            | Self::Value { replace_len, .. }
            | Self::OrderGroup { replace_len, .. }
            | Self::Limit { replace_len, .. }
            | Self::General { replace_len, .. } => *replace_len,
            Self::None | Self::FilterStart => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlCursor {
    pub slot: CursorSlot,
    pub shape: StatementShape,
}

const COMPARISON_OPS: [&str; 7] = ["=", "<", ">", "<=", ">=", "<>", "!="];
const STOPS_TABLE: [&str; 12] =
    ["where", "on", "set", "group", "order", "having", "limit", "offset", "union", "returning", "values", "select"];
const BY_TERMINATORS: [&str; 7] = ["where", "from", "having", "limit", "offset", "union", "select"];
const LIMIT_TERMINATORS: [&str; 11] =
    ["select", "from", "where", "join", "group", "order", "having", "union", "values", "set", "into"];
const FILTER_START_KEYWORDS: [&str; 5] = ["where", "having", "on", "and", "or"];
// Usually followed by a column, table or expression.
const EXPRESSION_LEAD_WORDS: [&str; 24] = [
    "from", "join", "on", "where", "and", "or", "not", "having", "by", "set", "select", "in", "like", "ilike",
    "regexp", "rlike", "glob", "match", "to", "between", "distinct", "when", "then", "else",
];

fn is_comparison(t: Option<&Token>) -> bool {
    t.is_some_and(|t| t.kind == TokenKind::Op && COMPARISON_OPS.contains(&t.text))
}

#[derive(Default)]
struct Markers {
    // Last FROM, JOIN, INTO or UPDATE.
    src: Option<usize>,
    // `by` of the last ORDER BY or GROUP BY.
    by: Option<usize>,
    by_group: bool,
    // Last LIMIT or OFFSET.
    limit: Option<usize>,
    offset_seen: bool,
    // Last UPDATE's SET.
    set: Option<usize>,
    // `into` of the last INSERT INTO or REPLACE INTO.
    into: Option<usize>,
    select_seen: bool,
    delete_seen: bool,
}

fn compute_shape(code: &[&Token], partial: Option<usize>) -> (StatementShape, Markers) {
    let mut m = Markers::default();
    let mut s = StatementShape::default();
    let prev_is = |i: usize, word: &str| i > 0 && code[i - 1].is_keyword(word);
    for (i, t) in code.iter().enumerate() {
        if t.kind != TokenKind::Ident || Some(i) == partial {
            continue;
        }
        match t.lower.as_str() {
            "from" => {
                if !prev_is(i, "distinct") {
                    s.has_from = true;
                    m.src = Some(i);
                }
            }
            "join" => {
                s.has_join = true;
                m.src = Some(i);
            }
            "into" => {
                if prev_is(i, "insert") || prev_is(i, "replace") {
                    m.src = Some(i);
                    m.into = Some(i);
                }
            }
            "update" => {
                s.has_update = true;
                m.src = Some(i);
            }
            "where" => {
                s.in_where = true;
                s.in_filter_clause = true;
            }
            "having" | "on" => s.in_filter_clause = true,
            "select" => {
                m.select_seen = true;
                if s.has_insert {
                    s.insert_body = true;
                }
            }
            "values" => {
                if s.has_insert {
                    s.insert_body = true;
                }
            }
            "insert" | "replace" => s.has_insert = true,
            "delete" => m.delete_seen = true,
            "case" => s.has_case = true,
            "set" => {
                if s.has_update {
                    m.set = Some(i);
                }
            }
            "by" => {
                if prev_is(i, "order") || prev_is(i, "group") {
                    m.by = Some(i);
                    m.by_group = prev_is(i, "group");
                    if m.by_group {
                        s.group_by_seen = true;
                    } else {
                        s.order_by_seen = true;
                    }
                }
            }
            "limit" | "offset" => {
                m.limit = Some(i);
                if t.lower == "offset" {
                    m.offset_seen = true;
                }
            }
            _ => {}
        }
    }
    s.at_statement_start = code.is_empty() || (code.len() == 1 && partial == Some(0));
    s.in_select_list = m.select_seen && !s.has_from;
    s.returning_slot = s.insert_body || m.delete_seen || (s.has_update && m.set.is_some());
    s.joinable = s.has_from && !s.in_where;
    (s, m)
}

fn expects_expression(prev: Option<&Token>) -> bool {
    let Some(prev) = prev else { return true };
    match prev.kind {
        TokenKind::Punct => prev.text == "," || prev.text == "(",
        TokenKind::Op => COMPARISON_OPS.contains(&prev.text),
        TokenKind::Ident => EXPRESSION_LEAD_WORDS.contains(&prev.lower.as_str()),
        _ => false,
    }
}

fn lower_ident(t: &Token) -> String {
    t.ident_text().to_lowercase()
}

pub fn analyze_cursor(before: &str, driver: Option<&DriverType>) -> SqlCursor {
    let all = tokenize(before, driver);
    let len = before.len();

    // Only an unclosed comment puts the cursor inside a comment. One that closes at the cursor is code.
    if let Some(last) = all.last()
        && last.kind == TokenKind::Comment
        && last.end >= len
        && last.unterminated
    {
        return SqlCursor { slot: CursorSlot::None, shape: compute_shape(&[], None).0 };
    }

    let code: Vec<&Token> = all.iter().filter(|t| t.kind != TokenKind::Comment).collect();
    let at = |i: isize| -> Option<&Token> { usize::try_from(i).ok().and_then(|i| code.get(i).copied()) };

    // Token under the cursor that's still being typed.
    let mut partial: Option<usize> = None;
    if let Some(last) = code.last()
        && last.end >= len
    {
        if matches!(last.kind, TokenKind::Ident | TokenKind::Number)
            || (last.kind == TokenKind::Quoted && last.unterminated)
        {
            partial = Some(code.len() - 1);
        } else if last.kind == TokenKind::String && last.unterminated {
            // Inside a string literal, only offer values after a comparison.
            let (shape, _) = compute_shape(&code, None);
            if is_comparison(at(code.len() as isize - 2)) {
                let prefix = before[last.start + 1..].to_lowercase();
                return SqlCursor { slot: CursorSlot::Value { prefix, replace_len: len - last.start }, shape };
            }
            return SqlCursor { slot: CursorSlot::None, shape };
        }
    }

    let (mut shape, m) = compute_shape(&code, partial);
    let complete_end = if partial.is_some() { code.len() - 1 } else { code.len() };
    let ce = complete_end as isize;
    let prev = at(ce - 1);
    let partial_tok = partial.map(|p| code[p]);
    let prefix = partial_tok.map(lower_ident).unwrap_or_default();
    let replace_len = partial_tok.map_or(0, |p| len - p.start);
    shape.after_on_keyword = shape.has_join && (is_keyword(prev, "on") || partial_tok.is_some_and(|p| p.lower == "on"));

    // After `alias.` or `schema.table.`, with or without a partial word.
    if is_punct(prev, ".") && is_ident_like(at(ce - 2)) {
        let mut segments = vec![at(ce - 2).map(|t| t.text.to_string()).unwrap_or_default()];
        if is_punct(at(ce - 3), ".") && is_ident_like(at(ce - 4)) {
            segments.insert(0, at(ce - 4).map(|t| t.text.to_string()).unwrap_or_default());
        }
        let prefix = partial_tok.map(|p| p.text.to_string()).unwrap_or_default();
        return SqlCursor { slot: CursorSlot::Dot { segments, prefix, replace_len }, shape };
    }

    if is_comparison(prev) {
        return SqlCursor { slot: CursorSlot::Value { prefix, replace_len }, shape };
    }

    // Caret touching a just-typed clause keyword like `WHERE|` or `ORDER BY|`. Offer the clause body with
    // a leading space instead of filtering on the keyword as a prefix.
    if let Some(p) = partial_tok.filter(|p| p.kind == TokenKind::Ident) {
        let kw = p.lower.as_str();
        let prev_kw = at(code.len() as isize - 2);
        if kw == "by" && (is_keyword(prev_kw, "order") || is_keyword(prev_kw, "group")) {
            let slot = CursorSlot::OrderGroup {
                group: is_keyword(prev_kw, "group"),
                expects_expr: true,
                direction_allowed: false,
                trailing_keywords: Vec::new(),
                prefix: String::new(),
                replace_len: 0,
                leading_space: true,
            };
            return SqlCursor { slot, shape };
        }
        if kw == "set" && shape.has_update {
            let slot = CursorSlot::SetColumn { prefix: String::new(), replace_len: 0, leading_space: true };
            return SqlCursor { slot, shape };
        }
        if FILTER_START_KEYWORDS.contains(&kw) || (kw == "from" && is_keyword(prev_kw, "distinct")) {
            return SqlCursor { slot: CursorSlot::FilterStart, shape };
        }
        if kw == "from" || kw == "join" {
            let slot = CursorSlot::Table { prefix: String::new(), replace_len: 0, leading_space: true };
            return SqlCursor { slot, shape };
        }
    }

    // UPDATE ... SET list, at a column position right after SET or a comma.
    if let Some(set) = m.set
        && !shape.in_where
    {
        let mut fresh = true;
        for t in &code[set + 1..complete_end] {
            if t.is_punct(",") {
                fresh = true;
            } else if t.kind == TokenKind::Op && t.text == "=" {
                fresh = false;
            }
        }
        if fresh {
            return SqlCursor { slot: CursorSlot::SetColumn { prefix, replace_len, leading_space: false }, shape };
        }
    }

    // Inside the column list of `INSERT INTO tbl (`.
    if let Some(into) = m.into {
        let mut i = into as isize + 1;
        if is_ident_like(at(i)) {
            i += 1;
            if is_punct(at(i), ".") && is_ident_like(at(i + 1)) {
                i += 2;
            }
            if is_punct(at(i), "(") {
                let mut open = true;
                let mut used = Vec::new();
                for j in (i + 1)..ce {
                    let t = at(j);
                    if is_punct(t, ")") {
                        open = false;
                        break;
                    }
                    if let Some(t) = t.filter(|t| t.is_ident_like())
                        && !is_punct(at(j + 1), ".")
                        && !is_punct(at(j - 1), ".")
                    {
                        used.push(lower_ident(t));
                    }
                }
                if open && i < ce {
                    return SqlCursor { slot: CursorSlot::InsertColumns { used, prefix, replace_len }, shape };
                }
            }
        }
    }

    // Right after the last FROM, JOIN, INTO or UPDATE, or after a comma in its table list.
    if let Some(src) = m.src {
        let stopped = code[(src + 1).min(complete_end)..complete_end]
            .iter()
            .any(|t| t.kind == TokenKind::Ident && STOPS_TABLE.contains(&t.lower.as_str()));
        if !stopped {
            let tail_len = complete_end.saturating_sub(src + 1);
            if tail_len == 0 || is_punct(at(ce - 1), ",") {
                return SqlCursor { slot: CursorSlot::Table { prefix, replace_len, leading_space: false }, shape };
            }
        }
    }

    // ORDER BY or GROUP BY list, until a later clause ends it.
    if let Some(by) = m.by {
        let mut terminated = false;
        let mut term_start = by + 1;
        for (i, t) in code.iter().enumerate().take(complete_end).skip(by + 1) {
            if t.kind == TokenKind::Ident && BY_TERMINATORS.contains(&t.lower.as_str()) {
                terminated = true;
                break;
            }
            if t.is_punct(",") {
                term_start = i + 1;
            }
        }
        if !terminated {
            let expects_expr = expects_expression(prev);
            let term_last = if term_start < complete_end { Some(code[complete_end - 1]) } else { None };
            let direction_allowed = !m.by_group
                && term_last.is_some_and(|t| {
                    !(t.kind == TokenKind::Ident && (t.lower == "asc" || t.lower == "desc"))
                        && (matches!(t.kind, TokenKind::Ident | TokenKind::Quoted | TokenKind::Number)
                            || t.is_punct(")"))
                });
            let trailing_keywords = if expects_expr {
                Vec::new()
            } else if m.by_group {
                vec!["HAVING", "ORDER BY", "LIMIT", "OFFSET"]
            } else {
                vec!["LIMIT", "OFFSET"]
            };
            let slot = CursorSlot::OrderGroup {
                group: m.by_group,
                expects_expr,
                direction_allowed,
                trailing_keywords,
                prefix,
                replace_len,
                leading_space: false,
            };
            return SqlCursor { slot, shape };
        }
    }

    // After LIMIT or OFFSET only a number fits, plus at most one OFFSET.
    if let Some(limit) = m.limit
        && Some(limit) != partial
    {
        let mut active = true;
        let mut limit_has_value = false;
        for (i, t) in code.iter().enumerate().skip(limit + 1) {
            if Some(i) == partial {
                continue;
            }
            if t.is_punct(")") || (t.kind == TokenKind::Ident && LIMIT_TERMINATORS.contains(&t.lower.as_str())) {
                active = false;
                break;
            }
            if t.kind == TokenKind::Number || (t.kind == TokenKind::Ident && t.lower == "all") {
                limit_has_value = true;
            }
        }
        if active {
            let offer_offset = code[limit].lower == "limit" && limit_has_value && !m.offset_seen;
            return SqlCursor { slot: CursorSlot::Limit { offer_offset, prefix, replace_len }, shape };
        }
    }

    // In a filter once WHERE, HAVING, ON, AND or OR appears in a statement that filters.
    let filter_keyword_seen = shape.in_filter_clause
        || code[..complete_end].iter().any(|t| t.kind == TokenKind::Ident && (t.lower == "and" || t.lower == "or"));
    let in_filter = filter_keyword_seen
        && ((shape.has_from && (m.select_seen || m.delete_seen)) || (shape.has_update && m.set.is_some()));
    let slot = CursorSlot::General { prefix, replace_len, in_filter, expects_expr: expects_expression(prev) };
    SqlCursor { slot, shape }
}
