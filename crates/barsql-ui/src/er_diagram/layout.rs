// Where the diagram puts its tables, and the keys between them. Sizes are rems of the 13px root.
use std::collections::HashMap;

use barsql_core::ColumnInfo;

pub const BOX_W: f32 = 16.923;
pub const HEADER_H: f32 = 2.154;
pub const ROW_H: f32 = 1.692;
const GAP_X: f32 = 6.154;
const GAP_Y: f32 = 1.846;
const MARGIN: f32 = 1.538;
// The layout aims for a diagram this much wider than tall, like the dialog it opens in.
const ASPECT: f32 = 1.6;
// Rounds of reordering, each one across the columns and back.
const SWEEPS: usize = 4;

pub struct ErTable {
    pub name: String,
    // Keys first: primary, then foreign, then the rest in their own order.
    pub columns: Vec<ColumnInfo>,
}

impl ErTable {
    pub fn new(name: String, mut columns: Vec<ColumnInfo>) -> Self {
        columns.sort_by_key(|c| match (c.is_primary, c.is_foreign) {
            (true, _) => 0,
            (false, true) => 1,
            _ => 2,
        });
        Self { name, columns }
    }

    // The columns its box lists: all of them, or only the keys.
    pub fn shown(&self, keys_only: bool) -> &[ColumnInfo] {
        match keys_only {
            true => &self.columns[..self.columns.iter().take_while(|c| c.is_primary || c.is_foreign).count()],
            false => &self.columns,
        }
    }

    pub fn height(&self, keys_only: bool) -> f32 {
        HEADER_H + ROW_H * self.shown(keys_only).len() as f32
    }

    fn row(&self, column: &str, keys_only: bool) -> Option<usize> {
        let lower = column.to_lowercase();
        self.shown(keys_only).iter().position(|c| c.name.to_lowercase() == lower)
    }
}

// A foreign key from its column's row to the referenced column's row. None is the box's header, for a row it
// doesn't show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: (usize, Option<usize>),
    pub to: (usize, Option<usize>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub h: f32,
}

impl Placed {
    pub fn center(&self) -> (f32, f32) {
        (self.x + BOX_W / 2., self.y + self.h / 2.)
    }
}

pub struct Layout {
    pub boxes: Vec<Placed>,
    pub edges: Vec<Edge>,
    pub width: f32,
    pub height: f32,
}

impl Layout {
    // The edges at a table, either way, and the tables at their other ends.
    pub fn related(&self, table: usize) -> (Vec<usize>, Vec<usize>) {
        let edges: Vec<usize> = (0..self.edges.len())
            .filter(|&ix| self.edges[ix].from.0 == table || self.edges[ix].to.0 == table)
            .collect();
        let mut tables: Vec<usize> = edges
            .iter()
            .map(|&ix| &self.edges[ix])
            .map(|edge| if edge.from.0 == table { edge.to.0 } else { edge.from.0 })
            .filter(|&other| other != table)
            .collect();
        tables.sort_unstable();
        tables.dedup();
        (edges, tables)
    }
}

// SQLite leaves the referenced column empty when the key references the primary key.
fn edges(tables: &[ErTable], keys_only: bool) -> Vec<Edge> {
    let by_name: HashMap<String, usize> =
        tables.iter().enumerate().map(|(ix, t)| (t.name.to_lowercase(), ix)).collect();
    let mut out = Vec::new();
    for (from, table) in tables.iter().enumerate() {
        for column in table.columns.iter().filter(|c| c.is_foreign && !c.foreign_table.is_empty()) {
            let Some(&to) = by_name.get(&column.foreign_table.to_lowercase()) else { continue };
            let target = &tables[to];
            let to_row = match column.foreign_column.is_empty() {
                true => target.shown(keys_only).iter().position(|c| c.is_primary),
                false => target.row(&column.foreign_column, keys_only),
            };
            out.push(Edge { from: (from, table.row(&column.name, keys_only)), to: (to, to_row) });
        }
    }
    out
}

// Referenced tables stand left of the ones that reference them, a column past their deepest reference; a table only
// referenced stands next to its nearest referrer. Each column is ordered by where its neighbours sit, which uncrosses
// most keys, and split evenly when it would be too tall for the diagram to keep its shape. Tables without keys
// either way come last, in a grid. The order never depends on how tall the boxes are, so showing only the keys
// doesn't reshuffle them.
pub fn arrange(tables: &[ErTable], keys_only: bool) -> Layout {
    arrange_with(tables, keys_only, SWEEPS)
}

fn arrange_with(tables: &[ErTable], keys_only: bool, sweeps: usize) -> Layout {
    let edges = edges(tables, keys_only);
    let n = tables.len();
    // Name order to start with, so the result doesn't depend on the order tables were loaded in.
    let mut by_name: Vec<usize> = (0..n).collect();
    by_name.sort_by_cached_key(|&ix| (tables[ix].name.to_lowercase(), ix));
    let mut rank = vec![0; n];
    for (place, &ix) in by_name.iter().enumerate() {
        rank[ix] = place;
    }
    let (mut refs, mut back): (Vec<Vec<usize>>, Vec<Vec<usize>>) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for edge in edges.iter().filter(|e| e.from.0 != e.to.0) {
        if !refs[edge.from.0].contains(&edge.to.0) {
            refs[edge.from.0].push(edge.to.0);
            back[edge.to.0].push(edge.from.0);
        }
    }
    for list in refs.iter_mut().chain(back.iter_mut()) {
        list.sort_by_key(|&ix| rank[ix]);
    }
    let linked: Vec<bool> = (0..n).map(|ix| !refs[ix].is_empty() || !back[ix].is_empty()).collect();
    let mut depth: Vec<Option<usize>> = vec![None; n];
    for ix in (0..n).filter(|&ix| linked[ix]) {
        visit(ix, &refs, &mut depth, &mut Vec::new());
    }
    let depth: Vec<usize> = (0..n)
        .map(|ix| match refs[ix].is_empty() {
            true => back[ix].iter().filter_map(|&r| depth[r]).min().map_or(0, |nearest| nearest.saturating_sub(1)),
            false => depth[ix].unwrap_or(0),
        })
        .collect();

    let count = by_name.iter().filter(|&&ix| linked[ix]).map(|&ix| depth[ix] + 1).max().unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); count];
    for &ix in by_name.iter().filter(|&&ix| linked[ix]) {
        layers[depth[ix]].push(ix);
    }
    order_layers(&mut layers, &depth, &refs, &back, sweeps);
    let loose: Vec<usize> = by_name.into_iter().filter(|&ix| !linked[ix]).collect();

    let heights: Vec<f32> = tables.iter().map(|t| t.height(keys_only)).collect();
    let stack = pick_stack(&layers, &loose, &heights);
    let mut boxes = vec![Placed { x: 0., y: 0., h: 0. }; n];
    let mut x = MARGIN;
    let mut place = |column: &[usize], centred: bool, x: &mut f32| {
        let tall = stack_height(column.iter().map(|&ix| heights[ix]));
        let mut y = MARGIN + if centred { (stack - tall) / 2. } else { 0. };
        for &ix in column {
            boxes[ix] = Placed { x: *x, y, h: heights[ix] };
            y += heights[ix] + GAP_Y;
        }
        *x += BOX_W + GAP_X;
    };
    for layer in &layers {
        for column in chunks(layer, &heights, stack, true) {
            place(&column, true, &mut x);
        }
    }
    for column in chunks(&loose, &heights, stack, false) {
        place(&column, false, &mut x);
    }
    let width = if n == 0 { 2. * MARGIN } else { x - GAP_X + MARGIN };
    let height = boxes.iter().map(|b| b.y + b.h).fold(stack + MARGIN, f32::max) + MARGIN;
    Layout { boxes, edges, width, height }
}

// The longest path to a referenced table. A cycle stops at the table it came back to.
fn visit(ix: usize, refs: &[Vec<usize>], depth: &mut [Option<usize>], path: &mut Vec<usize>) -> usize {
    if let Some(d) = depth[ix] {
        return d;
    }
    if path.contains(&ix) {
        return 0;
    }
    path.push(ix);
    let d = refs[ix].iter().map(|&to| visit(to, refs, depth, path) + 1).max().unwrap_or(0);
    path.pop();
    depth[ix] = Some(d);
    d
}

// Barycenter sweeps: each table moves toward the mean place of its neighbours in the columns already ordered, first
// left to right by what it references, then back by what references it. Places are shares of a column's length, so
// columns of different lengths compare. Ties keep their order.
fn order_layers(layers: &mut [Vec<usize>], depth: &[usize], refs: &[Vec<usize>], back: &[Vec<usize>], sweeps: usize) {
    let n = depth.len();
    let mut place = vec![0f32; n];
    let settle = |layer: &[usize], place: &mut [f32]| {
        for (i, &ix) in layer.iter().enumerate() {
            place[ix] = (i as f32 + 0.5) / layer.len() as f32;
        }
    };
    for layer in layers.iter() {
        settle(layer, &mut place);
    }
    let reorder = |layer: &mut Vec<usize>, place: &mut [f32], neighbours: &dyn Fn(usize) -> Vec<usize>| {
        let key = |ix: usize| {
            let near = neighbours(ix);
            match near.is_empty() {
                true => place[ix],
                false => near.iter().map(|&other| place[other]).sum::<f32>() / near.len() as f32,
            }
        };
        let keys: HashMap<usize, f32> = layer.iter().map(|&ix| (ix, key(ix))).collect();
        layer.sort_by(|a, b| keys[a].total_cmp(&keys[b]));
        settle(layer, place);
    };
    for _ in 0..sweeps {
        for (l, layer) in layers.iter_mut().enumerate().skip(1) {
            let left = |ix: usize| refs[ix].iter().copied().filter(|&other| depth[other] < l).collect();
            reorder(layer, &mut place, &left);
        }
        for (l, layer) in layers.iter_mut().enumerate().rev().skip(1) {
            let right = |ix: usize| back[ix].iter().copied().filter(|&other| depth[other] > l).collect();
            reorder(layer, &mut place, &right);
        }
    }
}

fn stack_height(heights: impl Iterator<Item = f32>) -> f32 {
    let (sum, count) = heights.fold((0., 0), |(sum, count), h| (sum + h, count + 1));
    sum + GAP_Y * (count.max(1) - 1) as f32
}

// A column split into as few stacks as fit `stack`. Linked columns split evenly, so their parts stand side by side
// at about the same height. Loose tables fill each stack in turn.
fn chunks(column: &[usize], heights: &[f32], stack: f32, even: bool) -> Vec<Vec<usize>> {
    let mut greedy: Vec<Vec<usize>> = Vec::new();
    let mut used = f32::INFINITY;
    for &ix in column {
        let add = heights[ix] + GAP_Y;
        if used + heights[ix] > stack {
            greedy.push(Vec::new());
            used = 0.;
        }
        greedy.last_mut().unwrap().push(ix);
        used += add;
    }
    if !even || greedy.len() < 2 {
        return greedy;
    }
    let parts = greedy.len();
    let target = stack_height(column.iter().map(|&ix| heights[ix])) / parts as f32;
    let mut even_parts: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0.;
    for &ix in column {
        let part = even_parts.last().unwrap();
        if !part.is_empty() && used + heights[ix] / 2. > target && even_parts.len() < parts {
            even_parts.push(Vec::new());
            used = 0.;
        }
        even_parts.last_mut().unwrap().push(ix);
        used += heights[ix] + GAP_Y;
    }
    let fits = even_parts.iter().all(|part| stack_height(part.iter().map(|&ix| heights[ix])) <= stack + 0.01);
    if fits { even_parts } else { greedy }
}

// The stack height that brings the whole diagram closest to its aspect. Never below the tallest box or the tallest
// linked column when that one fits on its own.
fn pick_stack(layers: &[Vec<usize>], loose: &[usize], heights: &[f32]) -> f32 {
    let tallest = heights.iter().copied().fold(0f32, f32::max);
    let total = stack_height(heights.iter().copied()).max(tallest);
    let shape = |stack: f32| {
        let columns: usize = layers.iter().map(|layer| chunks(layer, heights, stack, false).len()).sum::<usize>()
            + chunks(loose, heights, stack, false).len();
        let width = columns as f32 * (BOX_W + GAP_X) - GAP_X + 2. * MARGIN;
        let height = stack + 2. * MARGIN;
        ((width / height) / ASPECT).ln().abs()
    };
    let mut best = (f32::INFINITY, total);
    let step = ROW_H * 2.;
    let mut stack = tallest;
    while stack < total + step {
        let score = shape(stack);
        if score < best.0 - 1e-4 {
            best = (score, stack);
        }
        stack += step;
    }
    best.1
}

// Tables whose name holds the query, ignoring case: the exact name first, then names that start with it, then the
// rest, each group by name.
pub fn find(tables: &[ErTable], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(u8, String, usize)> = tables
        .iter()
        .enumerate()
        .filter_map(|(ix, table)| {
            let name = table.name.to_lowercase();
            let rank = match () {
                _ if name == query => 0,
                _ if name.starts_with(&query) => 1,
                _ if name.contains(&query) => 2,
                _ => return None,
            };
            Some((rank, name, ix))
        })
        .collect();
    hits.sort();
    hits.into_iter().map(|(_, _, ix)| ix).collect()
}

#[cfg(test)]
mod tests {
    use barsql_core::ColumnInfo;

    use super::{BOX_W, ErTable, HEADER_H, Layout, ROW_H, SWEEPS, arrange, arrange_with, find};

    fn column(name: &str, primary: bool, references: Option<&str>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "integer".into(),
            is_primary: primary,
            is_foreign: references.is_some(),
            foreign_table: references.unwrap_or_default().into(),
            ..Default::default()
        }
    }

    fn table(name: &str, columns: Vec<ColumnInfo>) -> ErTable {
        ErTable::new(name.into(), columns)
    }

    fn x(layout: &Layout, ix: usize) -> f32 {
        layout.boxes[ix].x
    }

    fn overlaps(layout: &Layout) -> bool {
        let boxes = &layout.boxes;
        (0..boxes.len()).any(|a| {
            (a + 1..boxes.len()).any(|b| {
                let (p, q) = (boxes[a], boxes[b]);
                p.x < q.x + BOX_W && q.x < p.x + BOX_W && p.y < q.y + q.h && q.y < p.y + p.h
            })
        })
    }

    // Crossings between keys whose ends sit in neighbouring columns.
    fn crossings(layout: &Layout) -> usize {
        let ends: Vec<((f32, f32), (f32, f32))> = layout
            .edges
            .iter()
            .map(|e| (layout.boxes[e.from.0].center(), layout.boxes[e.to.0].center()))
            .filter(|((x1, _), (x2, _))| x1 != x2)
            .map(|(a, b)| if a.0 < b.0 { (a, b) } else { (b, a) })
            .collect();
        let mut count = 0;
        for (i, (a1, a2)) in ends.iter().enumerate() {
            for (b1, b2) in &ends[i + 1..] {
                if a1.0 == b1.0 && a2.0 == b2.0 && (a1.1 - b1.1) * (a2.1 - b2.1) < 0. {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn keys_come_first_and_keys_only_keeps_just_them() {
        let orders = table(
            "orders",
            vec![column("note", false, None), column("user_id", false, Some("users")), column("id", true, None)],
        );
        let names: Vec<&str> = orders.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "user_id", "note"]);
        assert_eq!(orders.shown(true).len(), 2);
        assert_eq!(orders.height(true), HEADER_H + 2. * ROW_H);
        assert_eq!(orders.height(false), HEADER_H + 3. * ROW_H);
    }

    #[test]
    fn referenced_tables_stand_left_and_loose_ones_last() {
        let tables = [
            table("orders", vec![column("id", true, None), column("user_id", false, Some("users"))]),
            table("users", vec![column("id", true, None)]),
            table("items", vec![column("order_id", false, Some("orders"))]),
            table("notes", vec![column("body", false, None)]),
        ];
        let layout = arrange(&tables, false);
        assert!(x(&layout, 1) < x(&layout, 0) && x(&layout, 0) < x(&layout, 2), "users, then orders, then items");
        assert!(x(&layout, 3) > x(&layout, 2), "a table with no keys comes last");
        assert_eq!(layout.edges.len(), 2);
        assert_eq!(layout.edges[0].to, (1, Some(0)), "SQLite's empty column means the primary key");
        assert_eq!(layout.related(0), (vec![0, 1], vec![1, 2]));
    }

    #[test]
    fn keys_attach_to_shown_rows_or_the_header() {
        let mut key = column("currency", false, Some("currencies"));
        key.foreign_column = "code".into();
        let currencies = table("currencies", vec![column("id", true, None), column("code", false, None)]);
        let tables = [currencies, table("prices", vec![key])];
        assert_eq!(arrange(&tables, false).edges[0].to, (0, Some(1)));
        assert_eq!(arrange(&tables, true).edges[0].to, (0, None), "keys only hides `code`");
    }

    #[test]
    fn a_cycle_still_lays_out() {
        let tables =
            [table("a", vec![column("b_id", false, Some("b"))]), table("b", vec![column("a_id", false, Some("a"))])];
        let layout = arrange(&tables, false);
        assert_ne!(x(&layout, 0), x(&layout, 1));
    }

    #[test]
    fn a_table_only_referenced_stands_next_to_its_referrer() {
        let tables = [
            table("a", vec![column("id", true, None)]),
            table("b", vec![column("a_id", false, Some("a"))]),
            table("c", vec![column("b_id", false, Some("b"))]),
            table("d", vec![column("c_id", false, Some("c")), column("tag_id", false, Some("tags"))]),
            table("tags", vec![column("id", true, None)]),
        ];
        let layout = arrange(&tables, false);
        assert_eq!(x(&layout, 4), x(&layout, 2), "tags sits one column left of d, beside c");
    }

    #[test]
    fn ordering_uncrosses_keys() {
        // By name alone, b_1 → a_2 and b_2 → a_1 cross.
        let tables = [
            table("a_1", vec![column("id", true, None)]),
            table("a_2", vec![column("id", true, None)]),
            table("b_1", vec![column("a_id", false, Some("a_2"))]),
            table("b_2", vec![column("a_id", false, Some("a_1"))]),
        ];
        assert_eq!(crossings(&arrange(&tables, false)), 0);
    }

    // A made-up shop: every table references one or two before it.
    fn shop(count: usize) -> Vec<ErTable> {
        (0..count)
            .map(|i| {
                let mut columns = vec![column("id", true, None)];
                if i > 0 {
                    columns.push(column("p_id", false, Some(&format!("t{:03}", (i * 7 + 3) % i))));
                }
                if i > 3 && i % 3 == 0 {
                    columns.push(column("q_id", false, Some(&format!("t{:03}", (i * 13 + 5) % i))));
                }
                columns.extend((0..i % 9).map(|c| column(&format!("c{c}"), false, None)));
                table(&format!("t{i:03}"), columns)
            })
            .collect()
    }

    #[test]
    fn sweeps_uncross_a_tangle() {
        // By name, each b references the a at the other end: every pair of keys crosses.
        let mut tables: Vec<ErTable> =
            (0..8).map(|i| table(&format!("a{i}"), vec![column("id", true, None)])).collect();
        tables
            .extend((0..8).map(|i| table(&format!("b{i}"), vec![column("a_id", false, Some(&format!("a{}", 7 - i)))])));
        assert!(crossings(&arrange_with(&tables, false, 0)) > 0);
        assert_eq!(crossings(&arrange_with(&tables, false, SWEEPS)), 0);
    }

    #[test]
    fn a_layout_doesnt_overlap_or_depend_on_load_order() {
        let tables = shop(40);
        let layout = arrange(&tables, false);
        assert!(!overlaps(&layout) && !overlaps(&arrange(&tables, true)));
        let mut reversed = shop(40);
        reversed.reverse();
        let again = arrange(&reversed, false);
        for (ix, table) in tables.iter().enumerate() {
            assert_eq!(layout.boxes[ix], again.boxes[39 - ix], "{} moved", table.name);
        }
    }

    #[test]
    fn a_big_schema_keeps_the_dialogs_shape_and_loose_tables_fill_a_grid() {
        let mut tables = shop(80);
        tables.extend((0..60).map(|i| table(&format!("z{i:02}"), vec![column("x", false, None)])));
        let layout = arrange(&tables, false);
        assert!(!overlaps(&layout));
        let aspect = layout.width / layout.height;
        assert!((1.0..2.6).contains(&aspect), "{} x {} is {aspect}", layout.width, layout.height);
    }

    #[test]
    fn search_ranks_exact_then_prefix_then_the_rest() {
        let tables = [
            table("order_items", vec![]),
            table("orders", vec![]),
            table("reorders", vec![]),
            table("Orders_Archive", vec![]),
            table("users", vec![]),
        ];
        assert_eq!(find(&tables, " ORDERS "), [1, 3, 2]);
        assert_eq!(find(&tables, "order"), [0, 1, 3, 2]);
        assert!(find(&tables, "  ").is_empty());
    }
}
