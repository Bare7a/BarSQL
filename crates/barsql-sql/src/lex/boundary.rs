// Where statements end. The run splitter and the editor's statement regions both read these boundaries, then
// trim and label the text between them their own way.

use super::prim::{
    at_line_start, block_comment_end, bracket_end, dash_comment_at, delimiter_line, dollar_quoted_end, dollar_tag_len,
    go_line, hash_comment_at, is_escape_string_prefix, line_comment_end, quote_end,
};
use super::rules::LexRules;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    // Editor statements, for run glyphs and run-at-cursor.
    Statements,
    // What goes to the server in one request. T-SQL sends whole GO batches, because variables live for the
    // batch. Every other dialect splits the same way in both modes.
    ExecutionUnits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Boundary {
    // A statement terminator spanning `at..at + len`. `custom` is a mysql client DELIMITER rather than `;`.
    Terminator { at: usize, len: usize, custom: bool },
    // A mysql client `DELIMITER` line spanning `at..end`.
    DelimiterLine { at: usize, end: usize },
    // A T-SQL `GO [n]` line spanning `at..end`.
    BatchSeparator { at: usize, end: usize, repeat: u32 },
}

pub(crate) fn boundaries(sql: &str, rules: &LexRules, mode: Mode) -> Vec<Boundary> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut delimiter = ";";
    let mut trigger = Complete::Start;
    let mut blocks = Blocks::default();
    let semicolons = !(rules.batch_separators && mode == Mode::ExecutionUnits);
    let mut i = 0;
    while i < n {
        let c = b[i];
        if rules.client_delimiters
            && (c == b'd' || c == b'D')
            && at_line_start(sql, i)
            && let Some((token, consumed)) = delimiter_line(&sql[i..])
        {
            out.push(Boundary::DelimiterLine { at: i, end: i + consumed });
            delimiter = token;
            i += consumed;
            continue;
        }
        if rules.batch_separators
            && (c == b'g' || c == b'G')
            && at_line_start(sql, i)
            && let Some((repeat, consumed)) = go_line(&sql[i..])
        {
            out.push(Boundary::BatchSeparator { at: i, end: i + consumed, repeat });
            blocks = Blocks::default();
            i += consumed;
            continue;
        }
        if dash_comment_at(b, i, rules.dash_needs_space) {
            i = line_comment_end(b, i + 2).unwrap_or(n);
            continue;
        }
        if hash_comment_at(b, i, rules.hash_comments) {
            i = line_comment_end(b, i + 1).unwrap_or(n);
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = block_comment_end(b, i, rules.nested_block_comments).unwrap_or(n);
            continue;
        }
        if let Some(end) = quoted_span_end(b, i, rules) {
            i = end.unwrap_or(n);
            track(&mut trigger, &mut blocks, rules, None);
            continue;
        }
        if semicolons && c == delimiter.as_bytes()[0] && b[i..].starts_with(delimiter.as_bytes()) {
            trigger = trigger.next(CompleteToken::Semi);
            let inside_trigger = rules.trigger_bodies && trigger != Complete::Start;
            let inside_block = rules.block_depth && mode == Mode::Statements && !blocks.ends_at_semicolon();
            if !inside_trigger && !inside_block {
                out.push(Boundary::Terminator { at: i, len: delimiter.len(), custom: delimiter != ";" });
                trigger = Complete::Start;
                blocks = Blocks::default();
            }
            i += delimiter.len();
            continue;
        }
        if words_matter(rules) && is_word_start(c, rules) {
            let end = word_end(b, i, rules);
            track(&mut trigger, &mut blocks, rules, Some(&b[i..end]));
            i = end;
            continue;
        }
        if !c.is_ascii_whitespace() {
            track(&mut trigger, &mut blocks, rules, None);
        }
        i += 1;
    }
    out
}

// Feeds a token to SQLite's trigger state and T-SQL's blocks, each only in its own dialect. `word` is None for
// any other token.
fn track(trigger: &mut Complete, blocks: &mut Blocks, rules: &LexRules, word: Option<&[u8]>) {
    if rules.trigger_bodies {
        *trigger = trigger.next(word.map_or(CompleteToken::Other, CompleteToken::word));
    }
    if rules.block_depth {
        blocks.token(word);
    }
}

// End of the string, quoted identifier or dollar quote opening at `i`, or None if nothing opens there. The
// inner None means the input ends inside it.
pub(crate) fn quoted_span_end(b: &[u8], i: usize, rules: &LexRules) -> Option<Option<usize>> {
    Some(match b[i] {
        b'\'' => {
            quote_end(b, i, b'\'', rules.backslash_escapes || (rules.escape_strings && is_escape_string_prefix(b, i)))
        }
        b'"' if rules.double_quote_strings => quote_end(b, i, b'"', rules.backslash_escapes),
        b'"' | b'`' => quote_end(b, i, b[i], rules.ident_backslash),
        b'[' if rules.bracket_idents => bracket_end(b, i),
        b'$' if rules.dollar_quotes => match dollar_tag_len(b, i) {
            Some(_) => dollar_quoted_end(b, i),
            None => Some(i + 1),
        },
        _ => return None,
    })
}

// Words are read whole for SQLite's trigger state, T-SQL's blocks, and where `$` inside a word mustn't open a
// dollar quote.
pub(crate) fn words_matter(rules: &LexRules) -> bool {
    rules.trigger_bodies || rules.block_depth || (rules.dollar_in_words && rules.dollar_quotes)
}

pub(crate) fn is_word_start(c: u8, rules: &LexRules) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 || (rules.at_hash_words && matches!(c, b'@' | b'#'))
}

pub(crate) fn word_end(b: &[u8], i: usize, rules: &LexRules) -> usize {
    let is_word = |w: u8| {
        w.is_ascii_alphanumeric()
            || matches!(w, b'_' | b'$')
            || w >= 0x80
            || (rules.at_hash_words && matches!(w, b'@' | b'#'))
    };
    i + b[i..].iter().position(|&w| !is_word(w)).unwrap_or(b.len() - i)
}

// Mirrors sqlite3_complete's states. Whitespace and comments don't change the state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Complete {
    Start,
    Normal,
    Explain,
    Create,
    Trigger,
    Semi,
    End,
}

#[derive(Clone, Copy)]
pub(crate) enum CompleteToken {
    Semi,
    Other,
    Explain,
    Create,
    Temp,
    Trigger,
    End,
}

impl CompleteToken {
    pub(crate) fn word(w: &[u8]) -> Self {
        let is = |k: &[u8]| w.eq_ignore_ascii_case(k);
        if is(b"END") {
            Self::End
        } else if is(b"CREATE") {
            Self::Create
        } else if is(b"TEMP") || is(b"TEMPORARY") {
            Self::Temp
        } else if is(b"TRIGGER") {
            Self::Trigger
        } else if is(b"EXPLAIN") {
            Self::Explain
        } else {
            Self::Other
        }
    }
}

impl Complete {
    // Start after a Semi means the statement is complete.
    pub(crate) fn next(self, token: CompleteToken) -> Self {
        match (self, token) {
            (Self::Trigger | Self::Semi, CompleteToken::Semi) => Self::Semi,
            (_, CompleteToken::Semi) => Self::Start,
            (Self::Semi, CompleteToken::End) => Self::End,
            (Self::Trigger | Self::Semi | Self::End, _) => Self::Trigger,
            (Self::Start, CompleteToken::Explain) => Self::Explain,
            (Self::Explain, CompleteToken::Other) => Self::Explain,
            (Self::Start | Self::Explain, CompleteToken::Create) => Self::Create,
            (Self::Create, CompleteToken::Temp) => Self::Create,
            (Self::Create, CompleteToken::Trigger) => Self::Trigger,
            _ => Self::Normal,
        }
    }
}

// T-SQL needs no `;`, so a `;` inside BEGIN ... END or CASE ... END belongs to the enclosing statement. A
// CREATE PROCEDURE, FUNCTION, TRIGGER or VIEW must be the only statement in its batch, so it runs to GO.
#[derive(Default)]
struct Blocks {
    depth: u32,
    // Leading words of the current statement, uppercased, to spot CREATE [OR ALTER] PROCEDURE.
    lead: Vec<String>,
    module: bool,
    after_begin: bool,
    after_end: bool,
}

const NOT_BLOCKS: [&str; 5] = ["TRAN", "TRANSACTION", "DISTRIBUTED", "DIALOG", "CONVERSATION"];
const MODULES: [&str; 5] = ["PROC", "PROCEDURE", "FUNCTION", "TRIGGER", "VIEW"];

impl Blocks {
    fn ends_at_semicolon(&mut self) -> bool {
        // `BEGIN;` and `END;` settle here: neither opens a block.
        if self.after_end {
            self.depth = self.depth.saturating_sub(1);
        }
        self.after_begin = false;
        self.after_end = false;
        self.depth == 0 && !self.module
    }

    // A word, or None for any other token.
    fn token(&mut self, word: Option<&[u8]>) {
        let is = |keyword: &str| word.is_some_and(|w| w.eq_ignore_ascii_case(keyword.as_bytes()));
        if std::mem::take(&mut self.after_begin) && !NOT_BLOCKS.iter().any(|k| is(k)) {
            self.depth += 1;
        }
        if std::mem::take(&mut self.after_end) {
            if is("CONVERSATION") {
                return;
            }
            self.depth = self.depth.saturating_sub(1);
            if is("TRY") || is("CATCH") {
                return;
            }
        }
        // Only a statement's first words are kept, so this allocates a few times per statement.
        if self.lead.len() < 4 {
            self.lead.push(word.map(|w| String::from_utf8_lossy(w).to_ascii_uppercase()).unwrap_or_default());
            self.module |= starts_module(&self.lead);
        }
        if is("BEGIN") {
            self.after_begin = true;
        } else if is("END") {
            self.after_end = true;
        } else if is("CASE") {
            self.depth += 1;
        }
    }
}

fn starts_module(lead: &[String]) -> bool {
    let words: Vec<&str> = lead.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["CREATE" | "ALTER", kind, ..] if MODULES.contains(kind) => true,
        ["CREATE", "OR", "ALTER", kind, ..] => MODULES.contains(kind),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cuts(sql: &str, rules: &LexRules, mode: Mode) -> Vec<Boundary> {
        boundaries(sql, rules, mode)
    }

    #[test]
    fn tsql_blocks_keep_their_semicolons_in_the_editor() {
        let rules = LexRules::TSQL;
        let semis = |sql: &str| {
            cuts(sql, &rules, Mode::Statements)
                .into_iter()
                .filter_map(|b| match b {
                    Boundary::Terminator { at, .. } => Some(at),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(semis("SELECT 1; SELECT 2;"), vec![8, 18]);
        assert_eq!(semis("IF 1=1 BEGIN SELECT 1; SELECT 2; END; SELECT 3;"), vec![36, 46]);
        assert_eq!(semis("BEGIN TRAN; UPDATE t SET a=1; COMMIT;"), vec![10, 28, 36]);
        assert_eq!(semis("BEGIN TRY SELECT 1; END TRY BEGIN CATCH SELECT 2; END CATCH; SELECT 3;"), vec![59, 69]);
        assert_eq!(semis("SELECT CASE WHEN a=1 THEN 2 END; SELECT 3;"), vec![31, 41]);
        assert_eq!(semis("CREATE PROCEDURE p AS SELECT 1; SELECT 2;"), Vec::<usize>::new());
        assert_eq!(semis("CREATE OR ALTER VIEW v AS SELECT 1; SELECT 2;"), Vec::<usize>::new());
        assert_eq!(semis("DECLARE @begin int = 1; SELECT @begin;"), vec![22, 37]);
        assert_eq!(semis("SELECT [a;b]; SELECT 2;"), vec![12, 22]);
        assert_eq!(semis("SELECT N'a;b'; SELECT 2;"), vec![13, 23]);
    }

    #[test]
    fn tsql_execution_units_are_go_batches() {
        let rules = LexRules::TSQL;
        let sql = "SELECT 1; SELECT 2\nGO\nSELECT 3\ngo 2\nSELECT 'x\nGO\n'";
        assert_eq!(
            cuts(sql, &rules, Mode::ExecutionUnits),
            vec![
                Boundary::BatchSeparator { at: 19, end: 22, repeat: 1 },
                Boundary::BatchSeparator { at: 31, end: 36, repeat: 2 },
            ]
        );
    }

    #[test]
    fn clickhouse_hash_comments_need_a_space_or_bang() {
        let rules = LexRules::CLICKHOUSE;
        assert_eq!(cuts("SELECT 1 # c;\n; SELECT 2", &rules, Mode::Statements).len(), 1);
        assert_eq!(cuts("SELECT 'a\\';b'; SELECT 2", &rules, Mode::Statements).len(), 1);
    }
}
