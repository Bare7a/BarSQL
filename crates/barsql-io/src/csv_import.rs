use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use barsql_core::DriverType;
use barsql_sql::dml::sql_type_for;
use serde::{Deserialize, Serialize};

use crate::csv::{CsvField, CsvReader, Source, csv_values};

const CANDIDATE_DELIMITERS: [char; 4] = [',', ';', '\t', '|'];
pub const PREVIEW_ROW_LIMIT: usize = 100;
// Bigger files skip the row count and the progress bar follows bytes instead.
pub const MAX_ROW_COUNT_BYTES: u64 = 64 << 20;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvOptions {
    // Empty means sniff it. Quoting is always the CSV standard.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub delimiter: String,
    #[serde(default)]
    pub has_header: bool,
    // Extra text that becomes SQL NULL. A bare empty field is NULL already.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub null_literal: String,
    // Leading lines dropped before the header is read.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skip_rows: usize,
    #[serde(default, skip_serializing_if = "is_false")]
    pub trim_space: bool,
}

pub fn new_reader<R: Read>(inner: R, opts: &CsvOptions) -> Result<CsvReader<R>, String> {
    let mut src = Source::new(inner);
    let head = src.peek(3).map_err(|e| e.to_string())?;
    if head == b"\xef\xbb\xbf" {
        src.discard(3);
    }
    // Skip before sniffing. A preamble has no delimiters and would make every candidate inconsistent.
    for _ in 0..opts.skip_rows {
        if !src.skip_line().map_err(|e| e.to_string())? {
            break;
        }
    }
    let comma = if opts.delimiter.is_empty() {
        let head = src.peek(64 * 1024).map_err(|e| e.to_string())?;
        sniff_delimiter(&String::from_utf8_lossy(head))
    } else {
        parse_delimiter(&opts.delimiter)?
    };
    Ok(CsvReader::from_source(src, comma, opts.trim_space))
}

// A candidate must split every sampled line into the same number of fields, and more than one. An
// absent character is "consistent" at one field and would otherwise win.
pub fn sniff_delimiter(sample: &str) -> char {
    let lines = sample_lines(sample, 5);
    if lines.is_empty() {
        return ',';
    }
    let mut best = (',', 1);
    for d in CANDIDATE_DELIMITERS {
        let counts: Vec<usize> = lines.iter().map(|line| count_fields(line, d)).collect();
        if counts.iter().all(|&n| n == counts[0]) && counts[0] > best.1 {
            best = (d, counts[0]);
        }
    }
    best.0
}

fn sample_lines(sample: &str, max: usize) -> Vec<String> {
    let normalized = sample.replace("\r\n", "\n");
    let raw: Vec<&str> = normalized.split('\n').collect();
    let mut out = Vec::new();
    for (i, line) in raw.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        if i == raw.len() - 1 && !sample.ends_with('\n') && !out.is_empty() {
            break;
        }
        out.push(line.to_string());
        if out.len() == max {
            break;
        }
    }
    out
}

fn count_fields(line: &str, delim: char) -> usize {
    let mut fields = 1;
    let mut in_quotes = false;
    for c in line.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == delim && !in_quotes {
            fields += 1;
        }
    }
    fields
}

pub fn parse_delimiter(s: &str) -> Result<char, String> {
    match s {
        "\\t" | "\t" => return Ok('\t'),
        "\\\\" => return Ok('\\'),
        _ => {}
    }
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c != '\u{fffd}' => Ok(c),
        _ => Err(format!("delimiter must be a single character, got {s:?}")),
    }
}

pub const INFER_BOOL: &str = "bool";
pub const INFER_INT: &str = "int";
pub const INFER_FLOAT: &str = "float";
pub const INFER_DATE: &str = "date";
pub const INFER_TIMESTAMP: &str = "timestamp";
pub const INFER_TEXT: &str = "text";

const INFER_ORDER: [&str; 5] = [INFER_BOOL, INFER_INT, INFER_FLOAT, INFER_DATE, INFER_TIMESTAMP];

pub fn infer_column_type<S: AsRef<str>>(samples: &[S]) -> &'static str {
    let mut fits = [true; 5];
    let mut seen = false;
    for raw in samples {
        let v = raw.as_ref().trim();
        if v.is_empty() {
            continue;
        }
        seen = true;
        for (i, t) in INFER_ORDER.iter().enumerate() {
            if fits[i] && !matches_type(t, v) {
                fits[i] = false;
            }
        }
    }
    if !seen {
        return INFER_TEXT;
    }
    INFER_ORDER.iter().zip(fits).find(|(_, fits)| *fits).map_or(INFER_TEXT, |(t, _)| t)
}

fn matches_type(t: &str, v: &str) -> bool {
    match t {
        INFER_BOOL => {
            matches!(v.to_lowercase().as_str(), "true" | "false" | "t" | "f" | "yes" | "no" | "y" | "n" | "0" | "1")
        }
        INFER_INT => parse_int(v),
        INFER_FLOAT => parse_float(v),
        INFER_DATE => crate::time_layouts::matches_date(v),
        INFER_TIMESTAMP => crate::time_layouts::matches_timestamp(v),
        _ => false,
    }
}

fn parse_int(v: &str) -> bool {
    let digits = v.strip_prefix(['+', '-']).unwrap_or(v);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && v.parse::<i64>().is_ok()
}

// Accepts inf, infinity and nan, but rejects out-of-range values that Rust would parse as infinity.
pub(crate) fn parse_float(v: &str) -> bool {
    match v.parse::<f64>() {
        Ok(f) if f.is_finite() => true,
        Ok(_) => {
            let body = v.strip_prefix(['+', '-']).unwrap_or(v).to_ascii_lowercase();
            matches!(body.as_str(), "inf" | "infinity" | "nan")
        }
        Err(_) => false,
    }
}

// Blanks become col1..colN and repeats gain a suffix, so a CREATE TABLE cannot collide.
pub fn unique_column_names(header: &[String]) -> Vec<String> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    header
        .iter()
        .enumerate()
        .map(|(i, raw)| {
            let mut name = raw.trim().to_string();
            if name.is_empty() {
                name = format!("col{}", i + 1);
            }
            let lower = name.to_lowercase();
            match seen.get(&lower).copied() {
                Some(n) => {
                    seen.insert(lower, n + 1);
                    format!("{name}_{}", n + 1)
                }
                None => {
                    seen.insert(lower, 1);
                    name
                }
            }
        })
        .collect()
}

pub fn positional_header(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("col{i}")).collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    // Driver-neutral types. sql_types holds the same ones in the connection's dialect.
    pub inferred_types: Vec<String>,
    pub sql_types: Vec<String>,
    // What was actually used, so a sniffed one shows in the dialog.
    pub delimiter: String,
    pub total_bytes: u64,
    pub truncated: bool,
    // 0 when the file was too large to count.
    pub total_rows: u64,
}

pub fn preview_file(path: &Path, driver: &DriverType, opts: &CsvOptions) -> Result<ImportPreview, String> {
    if path.as_os_str().is_empty() {
        return Err("no file selected".into());
    }
    let mut file = File::open(path).map_err(|e| open_error(path, &e))?;
    let total_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    let total_rows = count_rows(&mut file, opts, total_bytes);
    let mut reader = new_reader(file, opts)?;
    let Sample { columns, rows, truncated } = read_sample(&mut reader, opts.has_header, PREVIEW_ROW_LIMIT)?;
    if columns.is_empty() {
        return Err("the file has no columns".into());
    }
    let inferred: Vec<String> = (0..columns.len())
        .map(|i| infer_column_type(&rows.iter().filter_map(|r| r.get(i)).collect::<Vec<_>>()).to_string())
        .collect();
    let sql_types = inferred.iter().map(|t| sql_type_for(driver, t).to_string()).collect();
    Ok(ImportPreview {
        columns,
        rows,
        inferred_types: inferred,
        sql_types,
        delimiter: reader.comma().to_string(),
        total_bytes,
        truncated,
        total_rows,
    })
}

// Formatted as "open <path>: <reason>".
pub fn open_error(path: &Path, err: &io::Error) -> String {
    let reason = match err.kind() {
        io::ErrorKind::NotFound => "no such file or directory".to_string(),
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => err.to_string(),
    };
    format!("open {}: {reason}", path.display())
}

// Uses the import's own reader settings. 0 when the file is too big to scan twice or a read fails part-way.
// Always rewinds the file.
pub fn count_rows(file: &mut File, opts: &CsvOptions, total_bytes: u64) -> u64 {
    let count = (|| -> Option<u64> {
        if total_bytes > MAX_ROW_COUNT_BYTES {
            return None;
        }
        file.seek(SeekFrom::Start(0)).ok()?;
        let mut reader = new_reader(&mut *file, opts).ok()?;
        if opts.has_header {
            reader.read().ok()?;
        }
        let mut rows = 0;
        loop {
            match reader.read() {
                Ok(Some(_)) => rows += 1,
                Ok(None) => return Some(rows),
                Err(_) => return None,
            }
        }
    })();
    let _ = file.seek(SeekFrom::Start(0));
    count.unwrap_or(0)
}

struct Sample {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    truncated: bool,
}

fn read_sample<R: Read>(reader: &mut CsvReader<R>, has_header: bool, limit: usize) -> Result<Sample, String> {
    let first = match reader.read() {
        Ok(Some(first)) => csv_values(first),
        Ok(None) => return Err("the file is empty".into()),
        Err(err) => return Err(err.to_string()),
    };
    let mut rows = Vec::new();
    let columns = if has_header {
        unique_column_names(&first)
    } else {
        let columns = positional_header(first.len());
        rows.push(pad_row(&first, columns.len()));
        columns
    };
    while rows.len() < limit {
        match reader.read() {
            Ok(Some(record)) => rows.push(pad_row(&csv_values(record), columns.len())),
            // A malformed row shouldn't sink the preview, so show what there is.
            _ => return Ok(Sample { columns, rows, truncated: false }),
        }
    }
    let truncated = matches!(reader.read(), Ok(Some(_)));
    Ok(Sample { columns, rows, truncated })
}

fn pad_row(row: &[String], width: usize) -> Vec<String> {
    (0..width).map(|i| row.get(i).cloned().unwrap_or_default()).collect()
}

pub fn fields_values(record: &[CsvField]) -> Vec<String> {
    csv_values(record)
}

fn is_zero(v: &usize) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_delimiters() {
        let cases = [
            ("a,b,c\n1,2,3\n", ','),
            ("a;b;c\n1;2;3\n", ';'),
            ("a\tb\tc\n1\t2\t3\n", '\t'),
            ("a|b|c\n1|2|3\n", '|'),
            ("a;b\n\"x,y,z\";2\n\"p,q,r\";4\n", ';'),
            ("a,b\tc\n1,2,3\td\n", '\t'),
            ("a,b\n1,2,3\n", ','),
            ("onlyone\nvalue\n", ','),
            ("", ','),
            ("a;b;c\n1;2;3\n4;5", ';'),
        ];
        for (input, want) in cases {
            assert_eq!(sniff_delimiter(input), want, "{input:?}");
        }
    }

    #[test]
    fn parses_delimiters() {
        for (input, want) in [(",", ','), (";", ';'), ("\\t", '\t'), ("\t", '\t'), ("|", '|')] {
            assert_eq!(parse_delimiter(input), Ok(want));
        }
        for bad in ["", "ab", "::"] {
            assert!(parse_delimiter(bad).is_err(), "{bad:?}");
        }
    }

    fn read_all(input: &str, opts: &CsvOptions) -> (char, Vec<Vec<String>>) {
        let mut reader = new_reader(input.as_bytes(), opts).unwrap();
        let mut out = Vec::new();
        while let Some(fields) = reader.read().unwrap() {
            out.push(csv_values(fields));
        }
        (reader.comma(), out)
    }

    #[test]
    fn strips_the_bom_skips_rows_and_sniffs() {
        let (_, records) = read_all(
            "\u{feff}preamble line\nname,age\nAlice,30\n",
            &CsvOptions { skip_rows: 1, has_header: true, ..Default::default() },
        );
        assert_eq!(records, vec![vec!["name", "age"], vec!["Alice", "30"]]);
        let (comma, records) = read_all("a;b\n1;2\n", &CsvOptions { has_header: true, ..Default::default() });
        assert_eq!((comma, records[0].len()), (';', 2));
        let (_, records) = read_all("a,b,c\n1,2\n3,4,5\n", &CsvOptions::default());
        assert_eq!(records.iter().map(Vec::len).collect::<Vec<_>>(), [3, 2, 3]);
    }

    #[test]
    fn infers_column_types() {
        let cases: [(&[&str], &str); 13] = [
            (&["1", "42", "-7"], INFER_INT),
            (&["1.5", "2", "-0.25"], INFER_FLOAT),
            (&["true", "false", "yes"], INFER_BOOL),
            (&["2026-01-02", "2026-12-31"], INFER_DATE),
            (&["2026-01-02 15:04:05", "2026-01-02T15:04:05"], INFER_TIMESTAMP),
            (&["alice", "bob"], INFER_TEXT),
            (&["0", "1", "1"], INFER_BOOL),
            (&["1", "2", "n/a"], INFER_TEXT),
            (&["1", "2.5"], INFER_FLOAT),
            (&["", "  ", "10"], INFER_INT),
            (&["", "  "], INFER_TEXT),
            (&[], INFER_TEXT),
            (&["007", "010"], INFER_INT),
        ];
        for (samples, want) in cases {
            assert_eq!(infer_column_type(samples), want, "{samples:?}");
        }
        assert_eq!(infer_column_type(&["1e400"]), INFER_TEXT);
    }

    #[test]
    fn unique_and_positional_names() {
        let header: Vec<String> = ["id", "name", "", "name", "NAME", "  spaced  "].map(String::from).to_vec();
        assert_eq!(unique_column_names(&header), ["id", "name", "col3", "name_2", "NAME_3", "spaced"]);
        assert_eq!(positional_header(3), ["col1", "col2", "col3"]);
        assert!(positional_header(0).is_empty());
    }
}
