// ClickHouse's TabSeparated rows: fields split on tabs, escapes undone, \N for NULL.

// The fields of one line, without its `\n`. A String can hold any bytes, so fields stay bytes.
pub(crate) fn fields(line: &[u8]) -> Vec<Option<Vec<u8>>> {
    line.split(|&b| b == b'\t').map(|raw| if raw == b"\\N" { None } else { Some(unescape(raw)) }).collect()
}

// Fields as text, for names, types and the catalog.
pub(crate) fn text_fields(line: &[u8]) -> Vec<Option<String>> {
    fields(line).into_iter().map(|f| f.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())).collect()
}

// \b \f \r \n \t \0 \' \\ come back as the character. An unknown escape keeps its backslash, as ClickHouse
// writes it only for those.
pub(crate) fn unescape(raw: &[u8]) -> Vec<u8> {
    if !raw.contains(&b'\\') {
        return raw.to_vec();
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().copied();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(b'b') => out.push(0x08),
            Some(b'f') => out.push(0x0c),
            Some(b'r') => out.push(b'\r'),
            Some(b'n') => out.push(b'\n'),
            Some(b't') => out.push(b'\t'),
            Some(b'0') => out.push(0),
            Some(b'\'') => out.push(b'\''),
            Some(b'\\') => out.push(b'\\'),
            Some(other) => out.extend([b'\\', other]),
            None => out.push(b'\\'),
        }
    }
    out
}

// A query that fails after rows went out ends the body with
// `\r\n__exception__\r\n<tag>\r\n<message>\n<length> <tag>\r\n__exception__\r\n`. TSV never holds a raw `\r`,
// so a line of just `\r` opens it. `tag` is the response's X-ClickHouse-Exception-Tag.
pub(crate) fn opens_exception(line: &[u8]) -> bool {
    line == b"\r"
}

// The message inside an exception block, from the lines after the one that opened it.
pub(crate) fn exception_message<'a>(mut lines: impl Iterator<Item = &'a [u8]>) -> String {
    let mut message: Vec<String> = Vec::new();
    let Some(first) = lines.next() else { return String::new() };
    if first != b"__exception__\r" {
        message.push(String::from_utf8_lossy(first).trim_end_matches('\r').to_string());
    }
    let tag = lines.next().map(|t| String::from_utf8_lossy(t).trim_end_matches('\r').to_string()).unwrap_or_default();
    for line in lines {
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\r');
        // `<length> <tag>` closes the message.
        if !tag.is_empty() && text.split_once(' ').is_some_and(|(n, t)| t == tag && n.parse::<usize>().is_ok()) {
            break;
        }
        message.push(text.to_string());
    }
    message.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_undo_every_escape() {
        assert_eq!(
            text_fields(b"1\tx\\ty\\n\t\\N\tab\\0\t\\b\\f\\r\\'\\\\\\q"),
            [Some("1".into()), Some("x\ty\n".into()), None, Some("ab\0".into()), Some("\u{8}\u{c}\r'\\\\q".into()),]
        );
        assert_eq!(text_fields(b""), [Some(String::new())], "one empty field");
        assert_eq!(text_fields(b"\\N\t"), [None, Some(String::new())]);
        assert_eq!(unescape("é\\\\".as_bytes()), "é\\".as_bytes());
        assert_eq!(fields(b"\xde\xad"), [Some(vec![0xde, 0xad])], "bytes stay bytes");
    }

    #[test]
    fn an_exception_block_gives_its_message() {
        let body: &[u8] = b"1\n2\n\r\n__exception__\r\nTAG\r\nCode: 395. DB::Exception: boom: line one\nline two\n38 TAG\r\n__exception__\r\n";
        let lines: Vec<&[u8]> = body.split(|&b| b == b'\n').collect();
        let open = lines.iter().position(|l| opens_exception(l)).unwrap();
        assert_eq!(open, 2);
        let message = exception_message(lines[open + 1..].iter().copied());
        assert_eq!(message, "Code: 395. DB::Exception: boom: line one\nline two");
    }
}
