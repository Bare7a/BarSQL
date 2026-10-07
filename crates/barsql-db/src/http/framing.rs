// Where a response body ends: after Content-Length bytes, after the last chunk, or when the server closes.

use super::{Head, HttpError};

// A chunk-size line or a trailer this long is something other than HTTP.
const MAX_LINE: usize = 8 * 1024;

pub(super) enum Step {
    Data(Vec<u8>),
    NeedMore,
    End,
}

pub(super) enum Framing {
    Length(u64),
    Chunked(Chunk),
    Close,
}

pub(super) enum Chunk {
    Size,
    Data(u64),
    // The CRLF after a chunk's data.
    DataEnd,
    Trailers,
    Done,
}

impl Framing {
    pub(super) fn for_response(method: &str, head: &Head) -> Result<Self, HttpError> {
        if method == "HEAD" || head.status == 204 || head.status == 304 {
            return Ok(Self::Length(0));
        }
        if let Some(encoding) = head.header("transfer-encoding") {
            let last = encoding.rsplit(',').next().unwrap_or_default().trim();
            return Ok(if last.eq_ignore_ascii_case("chunked") { Self::Chunked(Chunk::Size) } else { Self::Close });
        }
        match head.header("content-length") {
            Some(length) => length
                .trim()
                .parse()
                .map(Self::Length)
                .map_err(|_| HttpError::new(format!("the server sent an invalid Content-Length: {length}"))),
            None => Ok(Self::Close),
        }
    }

    pub(super) fn ends_at_close(&self) -> bool {
        matches!(self, Self::Close)
    }

    // Takes what belongs to the body from the front of `buf`.
    pub(super) fn step(&mut self, buf: &mut Vec<u8>) -> Result<Step, HttpError> {
        match self {
            Self::Length(0) => Ok(Step::End),
            Self::Length(remaining) => Ok(take(buf, remaining).map_or(Step::NeedMore, Step::Data)),
            Self::Close if buf.is_empty() => Ok(Step::NeedMore),
            Self::Close => Ok(Step::Data(std::mem::take(buf))),
            Self::Chunked(chunk) => loop {
                match chunk {
                    Chunk::Size => {
                        let Some(line) = line(buf)? else { return Ok(Step::NeedMore) };
                        // Extensions after `;` carry nothing we use.
                        let size = line.split(';').next().unwrap_or_default().trim();
                        let size = u64::from_str_radix(size, 16)
                            .map_err(|_| HttpError::new(format!("the server sent an invalid chunk size: {size}")))?;
                        *chunk = if size == 0 { Chunk::Trailers } else { Chunk::Data(size) };
                    }
                    Chunk::Data(remaining) => {
                        let Some(data) = take(buf, remaining) else { return Ok(Step::NeedMore) };
                        if *remaining == 0 {
                            *chunk = Chunk::DataEnd;
                        }
                        return Ok(Step::Data(data));
                    }
                    Chunk::DataEnd => {
                        if buf.len() < 2 {
                            return Ok(Step::NeedMore);
                        }
                        if !buf.starts_with(b"\r\n") {
                            return Err(HttpError::new("the server sent a malformed chunk"));
                        }
                        buf.drain(..2);
                        *chunk = Chunk::Size;
                    }
                    Chunk::Trailers => {
                        let Some(line) = line(buf)? else { return Ok(Step::NeedMore) };
                        if line.is_empty() {
                            *chunk = Chunk::Done;
                        }
                    }
                    Chunk::Done => return Ok(Step::End),
                }
            },
        }
    }
}

// Up to `remaining` bytes from `buf`, or None when it's empty.
fn take(buf: &mut Vec<u8>, remaining: &mut u64) -> Option<Vec<u8>> {
    if buf.is_empty() {
        return None;
    }
    let n = (*remaining).min(buf.len() as u64) as usize;
    *remaining -= n as u64;
    Some(buf.drain(..n).collect())
}

// A CRLF-terminated line from the front of `buf`, without the CRLF.
fn line(buf: &mut Vec<u8>) -> Result<Option<String>, HttpError> {
    match buf.windows(2).position(|w| w == b"\r\n") {
        Some(at) => {
            let text = String::from_utf8_lossy(&buf[..at]).into_owned();
            buf.drain(..at + 2);
            Ok(Some(text))
        }
        None if buf.len() > MAX_LINE => Err(HttpError::new("the server sent a malformed chunked response")),
        None => Ok(None),
    }
}
