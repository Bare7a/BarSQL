// Built-in functions per dialect, for completion and hover. The packs are static tables, so they cost no memory
// until a page of them is read. A live catalog from the server adds user functions, extensions and anything the
// packs miss.

use std::cmp::Ordering;
use std::collections::HashSet;

use barsql_core::{DriverType, FunctionInfo, FunctionKind, FunctionList, SqlDialect};

mod clickhouse;
mod core;
mod libsql;
mod mysql;
mod postgres;
mod sqlite;
mod tsql;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sig {
    // Without the parentheses.
    pub args: &'static str,
    pub returns: &'static str,
}

pub const fn sig(args: &'static str, returns: &'static str) -> Sig {
    Sig { args, returns }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinFn {
    // As the dialect's docs spell it: lowercase, T-SQL uppercase, ClickHouse camelCase.
    pub name: &'static str,
    pub kind: FunctionKind,
    // Overloads.
    pub sigs: &'static [Sig],
    // One English sentence.
    pub summary: &'static str,
    pub flags: u16,
    // The first release that has it, for display: "PostgreSQL 16". Empty when every supported release does.
    pub since: &'static str,
}

pub mod flags {
    // A special form that servers don't list as a function, like CAST or EXTRACT.
    pub const SYNTAX: u16 = 1;
    // Called without parentheses, like CURRENT_DATE.
    pub const NO_PARENS: u16 = 2;
    // A ClickHouse function whose name ignores case.
    pub const CI: u16 = 4;
    pub const MYSQL_ONLY: u16 = 8;
    pub const MARIADB_ONLY: u16 = 16;
    pub const DEPRECATED: u16 = 32;
}

const fn entry(kind: FunctionKind, name: &'static str, sigs: &'static [Sig], summary: &'static str) -> BuiltinFn {
    BuiltinFn { name, kind, sigs, summary, flags: 0, since: "" }
}

pub const fn scalar(name: &'static str, sigs: &'static [Sig], summary: &'static str) -> BuiltinFn {
    entry(FunctionKind::Scalar, name, sigs, summary)
}

pub const fn aggregate(name: &'static str, sigs: &'static [Sig], summary: &'static str) -> BuiltinFn {
    entry(FunctionKind::Aggregate, name, sigs, summary)
}

pub const fn window(name: &'static str, sigs: &'static [Sig], summary: &'static str) -> BuiltinFn {
    entry(FunctionKind::Window, name, sigs, summary)
}

pub const fn table(name: &'static str, sigs: &'static [Sig], summary: &'static str) -> BuiltinFn {
    entry(FunctionKind::Table, name, sigs, summary)
}

impl BuiltinFn {
    pub const fn flags(mut self, flags: u16) -> Self {
        self.flags = flags;
        self
    }

    pub const fn since(mut self, since: &'static str) -> Self {
        self.since = since;
        self
    }

    fn has(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

// The packs a driver reads, first match wins. A dialect pack overrides the shared core.
fn packs(driver: &DriverType) -> &'static [&'static [BuiltinFn]] {
    match driver {
        DriverType::Postgres => &[postgres::PACK, core::PACK],
        DriverType::MySql => &[mysql::PACK, core::PACK],
        DriverType::Sqlite => &[sqlite::PACK, core::PACK],
        DriverType::Turso => &[sqlite::PACK, libsql::PACK, core::PACK],
        DriverType::SqlServer => &[tsql::PACK, core::PACK],
        DriverType::ClickHouse => &[clickhouse::PACK, core::PACK],
        DriverType::Other(_) | DriverType::Unset => &[core::PACK],
    }
}

fn cmp_lower(name: &str, lower: &str) -> Ordering {
    name.bytes().map(|b| b.to_ascii_lowercase()).cmp(lower.bytes())
}

// Packs are sorted by lowercase name, so a lookup is a binary search.
fn find<'a>(pack: &'a [BuiltinFn], name: &str, exact_case: bool) -> Option<&'a BuiltinFn> {
    let lower = name.to_ascii_lowercase();
    let start = pack.partition_point(|f| cmp_lower(f.name, &lower) == Ordering::Less);
    pack[start..]
        .iter()
        .take_while(|f| f.name.eq_ignore_ascii_case(&lower))
        .find(|f| !exact_case || f.name == name || f.has(flags::CI))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallForm {
    // name(…)
    Args,
    // name()
    Empty,
    // CURRENT_DATE
    NoParens,
}

// Everything completion and hover show about one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDoc {
    pub name: String,
    // Set for a user function.
    pub schema: String,
    pub kind: FunctionKind,
    // (args, returns) per overload.
    pub signatures: Vec<(String, String)>,
    pub summary: String,
    // The server's own description, when it adds to the summary.
    pub description: String,
    pub since: &'static str,
    pub builtin: bool,
    pub call: CallForm,
    pub qualified_only: bool,
    pub source: String,
    // A ClickHouse combinator form: the base aggregate, then each suffix in the order applied with what it does.
    pub combinator: Option<(String, Vec<(String, &'static str)>)>,
    pub deprecated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindMask(u8);

impl KindMask {
    pub const SCALAR: Self = Self(1);
    pub const AGGREGATE: Self = Self(2);
    pub const WINDOW: Self = Self(4);
    pub const TABLE: Self = Self(8);
    pub const NONE: Self = Self(0);
    // Anything that yields a value inside an expression.
    pub const EXPR: Self = Self(1 | 2 | 4);

    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn allows(self, kind: FunctionKind) -> bool {
        let bit = match kind {
            FunctionKind::Scalar => 1,
            FunctionKind::Aggregate => 2,
            FunctionKind::Window => 4,
            FunctionKind::Table => 8,
        };
        self.0 & bit != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

// A completion match: tier, match quality (0 prefix, 1 substring) and the function.
pub type FunctionMatch = (u8, u8, FunctionDoc);

// Ranks within completion: user functions before curated built-ins before other server built-ins.
pub const TIER_USER: u8 = 2;
pub const TIER_BUILTIN: u8 = 5;
pub const TIER_SERVER: u8 = 6;

#[derive(Debug, Clone)]
struct ServerFn {
    info: FunctionInfo,
    lower: String,
}

#[derive(Debug, Clone)]
pub struct FunctionCatalog {
    packs: &'static [&'static [BuiltinFn]],
    // ClickHouse calls fail when the case is wrong.
    exact_case: bool,
    // Shared names render in the dialect's usual case: T-SQL writes them uppercase.
    upper_shared: bool,
    // Sorted by lowercase name.
    server: Vec<ServerFn>,
    // Lowercase names of every server built-in, when the server lists them all. A pack entry missing from it
    // isn't offered, since that server lacks it.
    live: Option<HashSet<String>>,
    combinators: Vec<String>,
    sqlite_modules: bool,
    // Some(true) on MariaDB and Some(false) on MySQL, which each lack the other's own functions. None when unknown.
    mariadb: Option<bool>,
}

impl Default for FunctionCatalog {
    fn default() -> Self {
        Self::builtin(&DriverType::Unset)
    }
}

impl FunctionCatalog {
    pub fn builtin(driver: &DriverType) -> Self {
        let dialect = driver.dialect();
        Self {
            packs: packs(driver),
            exact_case: dialect == Some(SqlDialect::ClickHouse),
            upper_shared: dialect == Some(SqlDialect::TSql),
            server: Vec::new(),
            live: None,
            combinators: clickhouse::COMBINATORS.iter().map(|(name, _)| name.to_string()).collect(),
            sqlite_modules: dialect == Some(SqlDialect::Sqlite),
            mariadb: None,
        }
    }

    // The packs plus what the server listed. Its combinators replace the static ClickHouse list.
    pub fn with_server(driver: &DriverType, list: FunctionList) -> Self {
        let mut catalog = Self::builtin(driver);
        let dialect = driver.dialect();
        let authoritative = matches!(dialect, Some(SqlDialect::Postgres | SqlDialect::Sqlite | SqlDialect::ClickHouse));
        let builtins: HashSet<String> =
            list.functions.iter().filter(|f| f.builtin).map(|f| f.name.to_ascii_lowercase()).collect();
        if authoritative && !builtins.is_empty() {
            catalog.live = Some(builtins);
        }
        if !list.combinators.is_empty() {
            catalog.combinators = list.combinators;
        }
        if dialect == Some(SqlDialect::MySql) {
            catalog.mariadb = Some(list.mariadb);
        }
        let mut server: Vec<ServerFn> =
            list.functions.into_iter().map(|info| ServerFn { lower: info.name.to_ascii_lowercase(), info }).collect();
        server.sort_by(|a, b| a.lower.cmp(&b.lower).then_with(|| a.info.schema.cmp(&b.info.schema)));
        catalog.server = server;
        catalog
    }

    // The shared core holds standard SQL names, which ClickHouse matches in any case too.
    fn exact_in(&self, pack_index: usize) -> bool {
        self.exact_case && pack_index + 1 < self.packs.len()
    }

    fn builtin_entry(&self, name: &str) -> Option<&'static BuiltinFn> {
        let mut packs = self.packs.iter().enumerate();
        packs.find_map(|(i, pack)| find(pack, name, self.exact_in(i)).filter(|f| self.offered(f)))
    }

    // A live list hides pack entries the server doesn't have, except what it never lists: special forms, SQLite's
    // table-valued modules, and ClickHouse combinator forms like countIf whose base it has.
    fn offered(&self, f: &BuiltinFn) -> bool {
        match self.mariadb {
            Some(true) if f.has(flags::MYSQL_ONLY) => return false,
            Some(false) if f.has(flags::MARIADB_ONLY) => return false,
            _ => {}
        }
        let Some(live) = &self.live else { return true };
        let combined = |base: &str| live.contains(&base.to_ascii_lowercase());
        f.has(flags::SYNTAX)
            || f.has(flags::NO_PARENS)
            || (self.sqlite_modules && f.kind == FunctionKind::Table)
            || live.contains(&f.name.to_ascii_lowercase())
            || (self.exact_case
                && self.combinators.iter().any(|c| f.name.strip_suffix(c.as_str()).is_some_and(combined)))
    }

    fn server_matches(&self, name: &str) -> impl Iterator<Item = &ServerFn> {
        let lower = name.to_ascii_lowercase();
        let start = self.server.partition_point(|f| f.lower < lower);
        let exact_case = self.exact_case;
        self.server[start..]
            .iter()
            .take_while(move |f| f.lower == lower)
            .filter(move |f| !exact_case || !f.info.case_sensitive || f.info.name == name)
    }

    // Whether the server accepts the name only as written, like most ClickHouse functions.
    pub fn case_sensitive(&self, name: &str) -> bool {
        if !self.exact_case {
            return false;
        }
        let in_packs =
            self.packs.iter().enumerate().any(|(i, pack)| {
                find(pack, name, self.exact_in(i)).is_some_and(|f| !self.exact_in(i) || f.has(flags::CI))
            });
        !in_packs && !self.server_matches(name).any(|f| !f.info.case_sensitive)
    }

    fn display_name(&self, f: &BuiltinFn, from_core: bool) -> String {
        if from_core && self.upper_shared { f.name.to_ascii_uppercase() } else { f.name.to_string() }
    }

    // The pack's docs win over the server's description, which says the same less carefully.
    fn builtin_doc(&self, f: &BuiltinFn, from_core: bool) -> FunctionDoc {
        let call = if f.has(flags::NO_PARENS) {
            CallForm::NoParens
        } else if !f.sigs.is_empty() && f.sigs.iter().all(|s| s.args.is_empty()) {
            CallForm::Empty
        } else {
            CallForm::Args
        };
        FunctionDoc {
            name: self.display_name(f, from_core),
            schema: String::new(),
            kind: f.kind,
            signatures: f.sigs.iter().map(|s| (s.args.to_string(), s.returns.to_string())).collect(),
            summary: f.summary.to_string(),
            description: String::new(),
            since: f.since,
            builtin: true,
            call,
            qualified_only: false,
            source: String::new(),
            combinator: None,
            deprecated: f.has(flags::DEPRECATED),
        }
    }

    fn server_doc(&self, f: &ServerFn) -> FunctionDoc {
        let info = &f.info;
        // A ClickHouse alias borrows the docs of what it aliases.
        let target = (!info.alias_to.is_empty()).then(|| self.builtin_entry(&info.alias_to)).flatten();
        let call = if info.signatures.iter().all(|s| s.args.is_empty()) && !info.signatures.is_empty() {
            CallForm::Empty
        } else {
            CallForm::Args
        };
        let mut doc = FunctionDoc {
            name: info.name.clone(),
            schema: if info.builtin { String::new() } else { info.schema.clone() },
            kind: info.kind,
            signatures: info.signatures.iter().map(|s| (s.args.clone(), s.returns.clone())).collect(),
            summary: String::new(),
            description: info.description.trim().to_string(),
            since: "",
            builtin: info.builtin,
            call,
            qualified_only: info.qualified_only,
            source: info.source.clone(),
            combinator: None,
            deprecated: false,
        };
        if let Some(target) = target {
            doc.summary = target.summary.to_string();
            if doc.signatures.is_empty() {
                doc.signatures = target.sigs.iter().map(|s| (s.args.to_string(), s.returns.to_string())).collect();
            }
            if doc.description.is_empty() {
                doc.description = format!("Alias of {}.", target.name);
            }
        }
        doc
    }

    // The function a call names. `schema` comes from a qualified call like pg_catalog.now().
    pub fn lookup(&self, schema: Option<&str>, name: &str) -> Option<FunctionDoc> {
        if let Some(schema) = schema.filter(|s| !s.is_empty()) {
            let user = self.server_matches(name).find(|f| f.info.schema.eq_ignore_ascii_case(schema));
            return user.map(|f| self.server_doc(f));
        }
        self.resolve(name, 0)
    }

    // An unqualified name. `depth` counts the combinator suffixes stripped so far.
    fn resolve(&self, name: &str, depth: usize) -> Option<FunctionDoc> {
        for (i, pack) in self.packs.iter().enumerate() {
            if let Some(f) = find(pack, name, self.exact_in(i)).filter(|f| self.offered(f)) {
                let from_core = i + 1 == self.packs.len();
                return Some(self.builtin_doc(f, from_core));
            }
        }
        // Prefer a built-in, then a function callable without its schema.
        let found = self.server_matches(name).min_by_key(|f| (!f.info.builtin, f.info.qualified_only));
        if let Some(f) = found {
            return Some(self.server_doc(f));
        }
        self.combinator(name, depth)
    }

    // ClickHouse aggregate combinators: sumIf, uniqState, quantilesMerge, avgOrNullIf... Each suffix wraps the
    // aggregate before it, so the last one is stripped first. Longer suffixes go first: MergeState before State.
    fn combinator(&self, name: &str, depth: usize) -> Option<FunctionDoc> {
        if !self.exact_case || depth == 3 {
            return None;
        }
        let mut suffixes: Vec<&String> =
            self.combinators.iter().filter(|c| name.len() > c.len() && name.ends_with(c.as_str())).collect();
        suffixes.sort_by_key(|c| std::cmp::Reverse(c.len()));
        for suffix in suffixes {
            let base = &name[..name.len() - suffix.len()];
            let Some(mut doc) = self.resolve(base, depth + 1).filter(|d| d.kind == FunctionKind::Aggregate) else {
                continue;
            };
            let about = clickhouse::COMBINATORS.iter().find(|(c, _)| c == suffix).map_or("", |(_, about)| *about);
            let (root, mut chain) = doc.combinator.take().unwrap_or_else(|| (doc.name.clone(), Vec::new()));
            chain.push((suffix.clone(), about));
            doc.combinator = Some((root, chain));
            doc.name = name.to_string();
            return Some(doc);
        }
        None
    }

    // Completion candidates whose name or one of its later words starts with `lc_prefix`, at most `limit` of
    // them. Docs are built only for the ones kept, since a short prefix matches hundreds.
    pub fn matches(&self, lc_prefix: &str, kinds: KindMask, limit: usize) -> Vec<FunctionMatch> {
        enum Found<'a> {
            Pack(&'static BuiltinFn, bool),
            Server(&'a ServerFn),
        }
        let mut found: Vec<(u8, u8, &str, Found)> = Vec::new();
        // Lowercase names already offered without a schema.
        let mut seen: HashSet<String> = HashSet::new();
        for (i, pack) in self.packs.iter().enumerate() {
            let from_core = i + 1 == self.packs.len();
            for f in pack.iter().filter(|f| kinds.allows(f.kind) && !f.has(flags::DEPRECATED)) {
                let Some(q) = word_match(f.name, lc_prefix) else { continue };
                if self.offered(f) && seen.insert(f.name.to_ascii_lowercase()) {
                    found.push((TIER_BUILTIN, q, f.name, Found::Pack(f, from_core)));
                }
            }
        }
        let server = self.server.iter().filter(|f| kinds.allows(f.info.kind));
        for f in server.clone().filter(|f| f.info.builtin) {
            let Some(q) = word_match(&f.info.name, lc_prefix) else { continue };
            if seen.insert(f.lower.clone()) {
                found.push((TIER_SERVER, q, &f.info.name, Found::Server(f)));
            }
        }
        // A function that overloads a name already offered adds nothing, unless it needs its schema. Extension
        // functions rank with the built-ins, so a big extension doesn't bury them.
        for f in server.filter(|f| !f.info.builtin && (f.info.qualified_only || !seen.contains(&f.lower))) {
            let Some(q) = word_match(&f.info.name, lc_prefix) else { continue };
            let tier = if f.info.source.is_empty() { TIER_USER } else { TIER_BUILTIN };
            found.push((tier, q, &f.info.name, Found::Server(f)));
        }
        found.sort_by(|a, b| (a.0, a.1, a.2.len(), a.2).cmp(&(b.0, b.1, b.2.len(), b.2)));
        found.truncate(limit);
        let doc = |found: Found| match found {
            Found::Pack(f, from_core) => self.builtin_doc(f, from_core),
            Found::Server(f) => self.server_doc(f),
        };
        found.into_iter().map(|(tier, q, _, f)| (tier, q, doc(f))).collect()
    }
}

// 0 when the name starts with the prefix, 1 when a later word does: after `_` or at a capital, as in
// jsonb_build_object and toStartOfDay. Ignores case and doesn't allocate.
fn word_match(name: &str, lc_prefix: &str) -> Option<u8> {
    let (n, p) = (name.as_bytes(), lc_prefix.as_bytes());
    let at =
        |i: usize| n.len() >= i + p.len() && n[i..i + p.len()].iter().zip(p).all(|(a, b)| a.to_ascii_lowercase() == *b);
    if at(0) {
        return Some(0);
    }
    let word_start = |i: usize| n[i - 1] == b'_' || (n[i].is_ascii_uppercase() && n[i - 1].is_ascii_lowercase());
    (1..n.len()).any(|i| word_start(i) && at(i)).then_some(1)
}

#[cfg(test)]
mod tests;
