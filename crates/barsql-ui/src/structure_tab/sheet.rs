use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use barsql_db::{ChunkBuilder, ColumnMeta};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::*;

use crate::grid::{Choices, Grid, TableOverlay};

// A cell as a section shows it. A flag draws as a checkbox.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    Text(String),
    Flag(bool),
}

pub(super) fn flag(value: Option<&str>) -> bool {
    value.is_some_and(|text| ["true", "t", "1", "yes"].iter().any(|word| text.trim().eq_ignore_ascii_case(word)))
}

pub(super) fn names(value: Option<&str>) -> Vec<String> {
    value.unwrap_or_default().split(',').map(str::trim).filter(|name| !name.is_empty()).map(String::from).collect()
}

// Each heading, and whether its column holds flags.
type Headings = Vec<(SharedString, bool)>;

// Each row's values as loaded, or None for an added row.
type Loaded = Vec<Option<Vec<Value>>>;

#[derive(Default)]
pub(super) struct Shape {
    pub columns: Headings,
    base: Loaded,
    shown: Vec<Vec<Value>>,
    dropped: HashSet<usize>,
    locked: HashSet<(usize, usize)>,
    pub choices: HashMap<usize, Choices>,
    pub cell_choices: HashMap<(usize, usize), Choices>,
    pub invalid: HashSet<(usize, usize)>,
    pub marks: HashMap<(usize, usize), Vec<Lucide>>,
}

impl Shape {
    pub fn new(columns: Headings) -> Self {
        Self { columns, ..Default::default() }
    }

    // The `locked` cells keep their values, and a dropped row keeps all of them.
    pub fn row(&mut self, base: Option<Vec<Value>>, shown: Vec<Value>, dropped: bool, locked: &[usize]) {
        let row = self.shown.len();
        let locked: Vec<usize> = if dropped { (0..shown.len()).collect() } else { locked.to_vec() };
        self.locked.extend(locked.into_iter().map(|column| (row, column)));
        if dropped {
            self.dropped.insert(row);
        }
        self.base.push(base);
        self.shown.push(shown);
    }
}

// The loaded values are the grid's rows and the draft's are staged over them, so a change shows as an edit does.
pub(super) struct Sheet {
    pub grid: Entity<Grid>,
    loaded: Option<(Headings, Loaded)>,
}

impl Sheet {
    pub fn new(cx: &mut App) -> Self {
        Self { grid: cx.new(|cx| Grid::new(Arc::from(Vec::new()), cx)), loaded: None }
    }

    pub fn show(&mut self, shape: Shape, editable: bool, cx: &mut App) {
        let mut staged = HashMap::new();
        for (row, (base, shown)) in shape.base.iter().zip(&shape.shown).enumerate() {
            for (column, value) in shown.iter().enumerate() {
                if base.as_ref().map(|base| &base[column]) != Some(value) {
                    let text = match value {
                        Value::Text(text) => text.clone(),
                        Value::Flag(on) => on.to_string(),
                    };
                    staged.insert((row, column), Some(text));
                }
            }
        }
        let added = shape.base.iter().enumerate().filter(|(_, base)| base.is_none()).map(|(row, _)| row).collect();
        // Reloading drops the selection, so the grid takes new rows only when the loaded ones change.
        let loaded = (shape.columns, shape.base);
        if self.loaded.as_ref() != Some(&loaded) {
            let (columns, rows) = &loaded;
            let meta: Vec<ColumnMeta> = columns
                .iter()
                .map(|(name, flag)| ColumnMeta {
                    name: name.to_string(),
                    type_name: if *flag { "boolean" } else { "" }.into(),
                    ..Default::default()
                })
                .collect();
            let mut builder = ChunkBuilder::new(columns.len(), rows.len());
            for row in rows {
                for column in 0..columns.len() {
                    match row.as_ref().map(|values| &values[column]) {
                        Some(Value::Text(text)) => builder.push_text(|s| s.push_str(text)),
                        Some(Value::Flag(on)) => builder.push_bool(*on),
                        None => builder.push_null(),
                    }
                }
                builder.end_row();
            }
            let chunk = Arc::new(builder.finish());
            self.grid.update(cx, |grid, cx| {
                grid.reset(meta.into(), cx);
                grid.push(chunk, cx);
            });
            self.loaded = Some(loaded);
        }
        let overlay = TableOverlay {
            staged: Arc::new(staged),
            deleted: shape.dropped,
            choices: shape.choices,
            cell_choices: shape.cell_choices,
            invalid: shape.invalid,
            marks: shape.marks,
            editable,
            added,
            fixed: true,
            locked: shape.locked,
            ..Default::default()
        };
        self.grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
    }
}
