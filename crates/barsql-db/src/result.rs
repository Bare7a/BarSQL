use std::sync::Arc;

use barsql_core::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}

// Used as the value's JSON type. The text is always the grid's display string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Null = 0,
    Text = 1,
    Number = 2,
    Bool = 3,
}

impl CellKind {
    fn from_bits(bits: u64) -> Self {
        match bits & 0b11 {
            1 => Self::Text,
            2 => Self::Number,
            3 => Self::Bool,
            _ => Self::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell<'a> {
    Null,
    Text(&'a str),
    Number(&'a str),
    Bool(bool),
}

impl<'a> Cell<'a> {
    pub fn display(self) -> Option<&'a str> {
        match self {
            Self::Null => None,
            Self::Text(s) | Self::Number(s) => Some(s),
            Self::Bool(true) => Some("true"),
            Self::Bool(false) => Some("false"),
        }
    }

    pub fn is_null(self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn to_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Text(s) => Value::Text(s.to_string()),
            Self::Bool(b) => Value::Bool(b),
            Self::Number(s) => match s.parse::<i64>() {
                Ok(i) => Value::Int(i),
                Err(_) => s.parse::<f64>().map_or_else(|_| Value::Text(s.to_string()), Value::Float),
            },
        }
    }
}

const CELLS_PER_WORD: usize = 32;

#[derive(Debug, Default)]
pub struct ResultChunk {
    columns: usize,
    rows: usize,
    text: String,
    ends: Vec<u32>,
    kinds: Vec<u64>,
}

impl ResultChunk {
    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell<'_> {
        let ix = row * self.columns + col;
        let kind = CellKind::from_bits(self.kinds[ix / CELLS_PER_WORD] >> ((ix % CELLS_PER_WORD) * 2));
        let start = if ix == 0 { 0 } else { self.ends[ix - 1] as usize };
        let text = &self.text[start..self.ends[ix] as usize];
        match kind {
            CellKind::Null => Cell::Null,
            CellKind::Text => Cell::Text(text),
            CellKind::Number => Cell::Number(text),
            CellKind::Bool => Cell::Bool(text == "true"),
        }
    }

    pub fn display(&self, row: usize, col: usize) -> Option<&str> {
        self.cell(row, col).display()
    }

    pub fn heap_bytes(&self) -> usize {
        self.text.capacity() + self.ends.capacity() * 4 + self.kinds.capacity() * 8
    }
}

pub struct ChunkBuilder {
    chunk: ResultChunk,
    cells: usize,
}

impl ChunkBuilder {
    pub fn new(columns: usize, rows: usize) -> Self {
        let cells = columns * rows;
        Self {
            chunk: ResultChunk {
                columns,
                rows: 0,
                text: String::with_capacity(cells * 8),
                ends: Vec::with_capacity(cells),
                kinds: Vec::with_capacity(cells.div_ceil(CELLS_PER_WORD)),
            },
            cells: 0,
        }
    }

    pub fn push_null(&mut self) {
        self.mark(CellKind::Null);
    }

    pub fn push_text(&mut self, write: impl FnOnce(&mut String)) {
        write(&mut self.chunk.text);
        self.mark(CellKind::Text);
    }

    pub fn push_number(&mut self, write: impl FnOnce(&mut String)) {
        write(&mut self.chunk.text);
        self.mark(CellKind::Number);
    }

    pub fn push_bool(&mut self, value: bool) {
        self.chunk.text.push_str(if value { "true" } else { "false" });
        self.mark(CellKind::Bool);
    }

    pub fn push_kind(&mut self, kind: CellKind, write: impl FnOnce(&mut String)) {
        if kind != CellKind::Null {
            write(&mut self.chunk.text);
        }
        self.mark(kind);
    }

    pub fn end_row(&mut self) {
        self.chunk.rows += 1;
    }

    pub fn rows(&self) -> usize {
        self.chunk.rows
    }

    pub fn is_empty(&self) -> bool {
        self.chunk.rows == 0
    }

    pub fn finish(self) -> ResultChunk {
        self.chunk
    }

    fn mark(&mut self, kind: CellKind) {
        let ix = self.cells;
        if ix / CELLS_PER_WORD >= self.chunk.kinds.len() {
            self.chunk.kinds.push(0);
        }
        self.chunk.kinds[ix / CELLS_PER_WORD] |= (kind as u64) << ((ix % CELLS_PER_WORD) * 2);
        self.chunk.ends.push(self.chunk.text.len() as u32);
        self.cells += 1;
    }
}

#[derive(Debug, Default, Clone)]
pub struct ResultSet {
    pub columns: Arc<[ColumnMeta]>,
    chunks: Vec<Arc<ResultChunk>>,
    starts: Vec<usize>,
    rows: usize,
}

impl ResultSet {
    pub fn new(columns: Arc<[ColumnMeta]>) -> Self {
        Self { columns, ..Default::default() }
    }

    pub fn push(&mut self, chunk: Arc<ResultChunk>) {
        self.starts.push(self.rows);
        self.rows += chunk.rows();
        self.chunks.push(chunk);
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell<'_> {
        if row >= self.rows || col >= self.columns.len() {
            return Cell::Null;
        }
        let Some(chunk_ix) = self.starts.partition_point(|&start| start <= row).checked_sub(1) else {
            return Cell::Null;
        };
        self.chunks[chunk_ix].cell(row - self.starts[chunk_ix], col)
    }

    pub fn display(&self, row: usize, col: usize) -> Option<&str> {
        self.cell(row, col).display()
    }

    pub fn heap_bytes(&self) -> usize {
        self.chunks.iter().map(|c| c.heap_bytes()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_round_trip_across_chunks_with_their_kinds() {
        let columns: Arc<[ColumnMeta]> = vec![
            ColumnMeta { name: "a".into(), type_name: "INTEGER".into() },
            ColumnMeta { name: "b".into(), type_name: "TEXT".into() },
        ]
        .into();
        assert_eq!(ResultSet::new(columns.clone()).cell(0, 0), Cell::Null);
        let mut set = ResultSet::new(columns);
        for chunk_rows in [40usize, 2] {
            let mut b = ChunkBuilder::new(2, chunk_rows);
            for r in 0..chunk_rows {
                let n = set.rows() + r;
                b.push_number(|s| s.push_str(&n.to_string()));
                match r % 3 {
                    0 => b.push_null(),
                    1 => b.push_bool(r % 2 == 0),
                    _ => b.push_text(|s| s.push('x')),
                }
                b.end_row();
            }
            set.push(Arc::new(b.finish()));
        }
        assert_eq!(set.rows(), 42);
        assert_eq!(set.cell(0, 0), Cell::Number("0"));
        assert_eq!(set.cell(0, 1), Cell::Null);
        assert_eq!(set.cell(1, 1), Cell::Bool(false));
        assert_eq!(set.cell(2, 1), Cell::Text("x"));
        assert_eq!(set.cell(37, 1), Cell::Bool(false));
        assert_eq!(set.cell(41, 0), Cell::Number("41"));
        assert_eq!(set.display(41, 1), Some("false"));
        assert_eq!(set.cell(42, 0), Cell::Null);
        assert_eq!(Cell::Number("9").to_value(), Value::Int(9));
        assert_eq!(Cell::Number("1.5").to_value(), Value::Float(1.5));
    }
}
