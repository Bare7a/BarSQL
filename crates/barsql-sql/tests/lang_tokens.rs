use barsql_core::DriverType;
use barsql_sql::lang::quoting::build_qualified_table;
use barsql_sql::lang::text::is_space;
use barsql_sql::lang::{Token, TokenKind, tokenize};
use barsql_sql::quote_ident;

fn last(sql: &str) -> Token<'_> {
    tokenize(sql, None).pop().expect("a token")
}

#[test]
fn tokenizes_a_representative_statement() {
    let tokens = tokenize("SELECT u.id, 'x' FROM users u -- t\nWHERE a >= 1", Some(&DriverType::Postgres));
    use TokenKind::*;
    let kinds: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        [Ident, Ident, Punct, Ident, Punct, String, Ident, Ident, Ident, Comment, Ident, Ident, Op, Number]
    );
    assert_eq!(tokens[0].lower, "select");
}

#[test]
fn marks_unterminated_strings_quoted_idents_and_comments() {
    for (sql, kind) in [
        ("SELECT 'ab", TokenKind::String),
        ("SELECT \"ab", TokenKind::Quoted),
        ("SELECT 1 /* x", TokenKind::Comment),
        ("SELECT 1 -- x", TokenKind::Comment),
    ] {
        let t = last(sql);
        assert_eq!((t.kind, t.unterminated), (kind, true), "{sql}");
    }
    assert!(!last("SELECT 1 -- x\n").unterminated);
}

#[test]
fn keeps_dollar_quoted_bodies_tokenized() {
    let tokens = tokenize("DO $$ SELECT 1 $$", Some(&DriverType::Postgres));
    assert_eq!(tokens.iter().map(|t| t.text).collect::<Vec<_>>(), ["DO", "$$", "SELECT", "1", "$$"]);
}

// Deterministic LCG, so failures reproduce from the seed.
fn lcg(mut seed: u32) -> impl FnMut() -> f64 {
    move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(seed) / 4_294_967_296.0
    }
}

const PIECES: [&str; 32] = [
    "'", "\"", "`", "$", "$$", "$tag$", "--", "#", "/*", "*/", "\\", ";", "\n", " ", ".", ",", "(", ")", "=", "<=",
    "<>", "::", "SELECT", "from", "users", "\"Us\"", "'it''s'", "E", "1.5e3", "$1", "абв", "🙂",
];

fn random_text(rnd: &mut impl FnMut() -> f64, max: f64, sep: &str) -> String {
    let n = 1 + (rnd() * max) as usize;
    (0..n).map(|_| PIECES[(rnd() * PIECES.len() as f64) as usize]).collect::<Vec<_>>().join(sep)
}

#[test]
fn fuzzed_input_never_panics_and_spans_are_well_formed() {
    let drivers = [Some(DriverType::Postgres), Some(DriverType::MySql), Some(DriverType::Sqlite), None];
    for seed in 1..=200 {
        let mut rnd = lcg(seed);
        let text = random_text(&mut rnd, 40.0, "");
        let driver = &drivers[(rnd() * drivers.len() as f64) as usize];
        let mut prev_end = 0;
        for t in tokenize(&text, driver.as_ref()) {
            let ctx = format!("seed={seed} driver={driver:?} text={text:?}");
            assert!(t.start >= prev_end && t.end > t.start && t.end <= text.len(), "{ctx}");
            assert_eq!(t.text, &text[t.start..t.end], "{ctx}");
            prev_end = t.end;
        }
    }
}

#[test]
fn skipped_gaps_are_only_whitespace() {
    for seed in 200..=300 {
        let mut rnd = lcg(seed);
        let text = random_text(&mut rnd, 30.0, " ");
        let mut pos = 0;
        for t in tokenize(&text, Some(&DriverType::Postgres)) {
            assert!(text[pos..t.start].chars().all(is_space), "seed={seed} text={text:?}");
            pos = t.end;
        }
        assert!(text[pos..].chars().all(is_space), "seed={seed}");
    }
}

#[test]
fn quote_ident_follows_the_driver() {
    assert_eq!(quote_ident(&DriverType::Postgres, "users"), "\"users\"");
    assert_eq!(quote_ident(&DriverType::Sqlite, "users"), "\"users\"");
    assert_eq!(quote_ident(&DriverType::MySql, "users"), "`users`");
    assert_eq!(quote_ident(&DriverType::Postgres, "a\"b"), "\"a\"\"b\"");
    assert_eq!(quote_ident(&DriverType::MySql, "a`b"), "`a``b`");
}

#[test]
fn build_qualified_table_drops_default_schemas() {
    assert_eq!(build_qualified_table(&DriverType::Postgres, "public", "users"), "\"public\".\"users\"");
    assert_eq!(build_qualified_table(&DriverType::MySql, "shop", "orders"), "`shop`.`orders`");
    assert_eq!(build_qualified_table(&DriverType::Sqlite, "main", "users"), "\"users\"");
    assert_eq!(build_qualified_table(&DriverType::Sqlite, "", "users"), "\"users\"");
    assert_eq!(build_qualified_table(&DriverType::Postgres, "", "users"), "\"users\"");
}
