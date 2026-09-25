use std::io::{self, Read};

const BUFFER_SIZE: usize = 64 * 1024;

// `quoted` tells an empty string ("") apart from an absent value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CsvField {
    pub value: String,
    pub quoted: bool,
}

pub fn csv_values(fields: &[CsvField]) -> Vec<String> {
    fields.iter().map(|f| f.value.clone()).collect()
}

// An invalid UTF-8 byte decodes as U+FFFD.
pub(crate) struct Source<R> {
    inner: R,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    consumed: u64,
}

impl<R: Read> Source<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self { inner, buf: Vec::with_capacity(BUFFER_SIZE), pos: 0, eof: false, consumed: 0 }
    }

    // Counts bytes read from `inner`, including ones still buffered.
    pub(crate) fn bytes_read(&self) -> u64 {
        self.consumed
    }

    pub(crate) fn peek(&mut self, n: usize) -> io::Result<&[u8]> {
        while self.buf.len() - self.pos < n && !self.eof {
            if self.pos > 0 {
                self.buf.drain(..self.pos);
                self.pos = 0;
            }
            let start = self.buf.len();
            self.buf.resize(start + BUFFER_SIZE, 0);
            let read = self.inner.read(&mut self.buf[start..]);
            match read {
                Ok(0) => {
                    self.buf.truncate(start);
                    self.eof = true;
                }
                Ok(read) => {
                    self.buf.truncate(start + read);
                    self.consumed += read as u64;
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => self.buf.truncate(start),
                Err(err) => {
                    self.buf.truncate(start);
                    return Err(err);
                }
            }
        }
        let end = (self.pos + n).min(self.buf.len());
        Ok(&self.buf[self.pos..end])
    }

    pub(crate) fn discard(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.buf.len());
    }

    pub(crate) fn read_char(&mut self) -> io::Result<Option<char>> {
        let head = self.peek(4)?;
        let Some(&first) = head.first() else {
            return Ok(None);
        };
        let width = match first {
            0x00..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 0,
        };
        let decoded = (width > 0 && head.len() >= width)
            .then(|| std::str::from_utf8(&head[..width]).ok().and_then(|s| s.chars().next()))
            .flatten();
        match decoded {
            Some(c) => {
                self.discard(width);
                Ok(Some(c))
            }
            None => {
                self.discard(1);
                Ok(Some('\u{fffd}'))
            }
        }
    }

    pub(crate) fn skip_line(&mut self) -> io::Result<bool> {
        loop {
            let chunk = self.peek(BUFFER_SIZE)?;
            if chunk.is_empty() {
                return Ok(false);
            }
            match chunk.iter().position(|&b| b == b'\n') {
                Some(ix) => {
                    self.discard(ix + 1);
                    return Ok(true);
                }
                None => {
                    let len = chunk.len();
                    self.discard(len);
                }
            }
        }
    }
}

// Lenient on purpose. Stray quotes are data and records may differ in field count.
pub struct CsvReader<R> {
    src: Source<R>,
    comma: char,
    trim: bool,
    pushback: Vec<char>,
    fields: Vec<CsvField>,
    sb: String,
}

impl<R: Read> CsvReader<R> {
    pub(crate) fn from_source(src: Source<R>, comma: char, trim: bool) -> Self {
        Self { src, comma, trim, pushback: Vec::new(), fields: Vec::new(), sb: String::new() }
    }

    pub fn comma(&self) -> char {
        self.comma
    }

    pub fn bytes_read(&self) -> u64 {
        self.src.bytes_read()
    }

    // Skips blank lines.
    pub fn read(&mut self) -> io::Result<Option<&[CsvField]>> {
        loop {
            match self.read_record()? {
                None => return Ok(None),
                Some(true) => continue,
                Some(false) => return Ok(Some(&self.fields)),
            }
        }
    }

    // Some(blank) for a record, None at the end.
    fn read_record(&mut self) -> io::Result<Option<bool>> {
        self.fields.clear();
        let mut consumed = false;
        // A line of only spaces is a one-empty-field record, not a blank line.
        let mut space_skipped = false;
        loop {
            self.sb.clear();
            if self.trim {
                self.skip_leading_space(&mut space_skipped)?;
                consumed = consumed || space_skipped;
            }
            let Some(c) = self.read_char()? else {
                if !consumed && self.fields.is_empty() {
                    return Ok(None);
                }
                self.fields.push(CsvField::default());
                return Ok(Some(false));
            };
            consumed = true;
            if c == '"' {
                let done = self.read_quoted()?;
                self.fields.push(CsvField { value: self.sb.clone(), quoted: true });
                if done {
                    return Ok(Some(false));
                }
                continue;
            }
            self.pushback.push(c);
            let first_field = self.fields.is_empty();
            let (done, ended_blank) = self.read_bare(first_field)?;
            self.fields.push(CsvField { value: self.sb.clone(), quoted: false });
            if done {
                return Ok(Some(ended_blank && !space_skipped));
            }
        }
    }

    // Leading space is trimmed even when the delimiter itself is whitespace.
    fn skip_leading_space(&mut self, skipped: &mut bool) -> io::Result<()> {
        loop {
            let Some(c) = self.read_char()? else { return Ok(()) };
            // A lone \r is whitespace, but \r\n ends the record.
            if !c.is_whitespace() || c == '\n' || (c == '\r' && self.peek_is_newline()?) {
                self.pushback.push(c);
                return Ok(());
            }
            *skipped = true;
        }
    }

    fn read_quoted(&mut self) -> io::Result<bool> {
        loop {
            // An unterminated quote ends the field rather than failing.
            let Some(c) = self.read_char()? else { return Ok(true) };
            if c != '"' {
                // A \r\n inside a quoted field folds to \n.
                if c == '\r' && self.peek_is_newline()? {
                    self.read_char()?;
                    self.sb.push('\n');
                } else {
                    self.sb.push(c);
                }
                continue;
            }
            let Some(next) = self.read_char()? else { return Ok(true) };
            if next == '"' {
                self.sb.push('"');
            } else if next == self.comma {
                return Ok(false);
            } else if next == '\n' {
                return Ok(true);
            } else if next == '\r' && self.peek_is_newline()? {
                self.read_char()?;
                return Ok(true);
            } else {
                // A bare quote mid-field is data.
                self.sb.push('"');
                self.pushback.push(next);
            }
        }
    }

    // (record done, lone terminator)
    fn read_bare(&mut self, first_field: bool) -> io::Result<(bool, bool)> {
        let mut empty = true;
        loop {
            let Some(c) = self.read_char()? else { return Ok((true, false)) };
            if c == self.comma {
                return Ok((false, false));
            } else if c == '\n' {
                return Ok((true, first_field && empty));
            } else if c == '\r' && self.peek_is_newline()? {
                self.read_char()?;
                return Ok((true, first_field && empty));
            } else {
                self.sb.push(c);
                empty = false;
            }
        }
    }

    fn read_char(&mut self) -> io::Result<Option<char>> {
        if let Some(c) = self.pushback.pop() {
            return Ok(Some(c));
        }
        let c = self.src.read_char()?;
        // A trailing \r before the end of input is dropped.
        if c == Some('\r') && self.src.peek(1)?.is_empty() {
            return Ok(None);
        }
        Ok(c)
    }

    fn peek_is_newline(&mut self) -> io::Result<bool> {
        let Some(c) = self.read_char()? else { return Ok(false) };
        self.pushback.push(c);
        Ok(c == '\n')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(input: &[u8]) -> Vec<Vec<CsvField>> {
        let mut reader = CsvReader::from_source(Source::new(input), ',', false);
        let mut out = Vec::new();
        while let Some(fields) = reader.read().unwrap() {
            out.push(fields.to_vec());
        }
        out
    }

    #[test]
    fn reports_quoting() {
        let records = read_all(b"a,,\"\",  \"x\" ,\"y\"\n");
        let got: Vec<(String, bool)> = records[0].iter().map(|f| (f.value.clone(), f.quoted)).collect();
        assert_eq!(
            got,
            [("a", false), ("", false), ("", true), ("  \"x\" ", false), ("y", true)].map(|(v, q)| (v.to_string(), q))
        );
    }

    #[test]
    fn invalid_utf8_reads_as_replacement_characters() {
        let records = read_all(b"\xff\xfe,x\n");
        assert_eq!(records[0][0].value, "\u{fffd}\u{fffd}");
    }
}
