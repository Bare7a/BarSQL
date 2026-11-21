// The diagram as a standalone SVG, from the canvas' layout.

use std::fmt::Write as _;

use barsql_core::SqlDialect;
use barsql_sql::display_type;
use gpui_kit::{Hsla, Rgba};

use super::layout::{BOX_W, ErTable, HEADER_H, Layout, ROW_H, Spot};

// One rem at the default zoom, and the margin around the diagram, in px.
const REM: f32 = 13.;
const MARGIN: f32 = 24.;
const FONT: &str = "Inter, -apple-system, 'Segoe UI', sans-serif";

pub struct Palette {
    pub background: Hsla,
    pub header: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub key: Hsla,
    pub link: Hsla,
    pub edge: Hsla,
}

pub struct Look<'a> {
    pub palette: Palette,
    pub icons: [&'a str; 2],
    // A text's width in px at a size, semibold or not.
    pub measure: &'a dyn Fn(&str, f32, bool) -> f32,
}

pub fn render(
    tables: &[ErTable],
    layout: &Layout,
    keys_only: bool,
    dialect: Option<SqlDialect>,
    look: &Look,
    rows: impl Fn(&str) -> Option<String>,
) -> String {
    let palette = &look.palette;
    let fit = |text: &str, room: f32, size: f32, bold: bool| fit(text, room, size, bold, look.measure);
    let (width, height) = (layout.width * REM + MARGIN * 2., layout.height * REM + MARGIN * 2.);
    let mut out = String::new();
    let _ = write!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0}" height="{height:.0}" viewBox="0 0 {width:.0} {height:.0}" font-family="{FONT}">"#
    );
    out.push_str("<defs>");
    for (id, icon) in ["key", "link"].iter().zip(look.icons) {
        let _ = write!(
            out,
            r#"<symbol id="{id}" viewBox="0 0 24 24"><g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</g></symbol>"#,
            inner(icon)
        );
    }
    out.push_str("</defs>");
    let _ = write!(out, r#"<rect width="100%" height="100%" {}/>"#, paint("fill", palette.background));
    let _ = write!(out, r#"<g transform="translate({MARGIN} {MARGIN})">"#);
    edges(&mut out, layout, palette);
    for (ix, (table, at)) in tables.iter().zip(&layout.boxes).enumerate() {
        let (x, y, w, h) = (at.x * REM, at.y * REM, BOX_W * REM, at.h * REM);
        let (pad, gap, header, side) = (0.615 * REM, 0.462 * REM, HEADER_H * REM, 0.923 * REM);
        // Clipped like the canvas' box.
        let _ = write!(
            out,
            r#"<clipPath id="box{ix}"><rect x="{x}" y="{y}" width="{w}" height="{h}" rx="6"/></clipPath><g clip-path="url(#box{ix})">"#
        );
        let _ = write!(
            out,
            r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="6" {}/>"#,
            paint("fill", palette.background)
        );
        let _ = write!(
            out,
            r#"<path d="M{x} {} a6 6 0 0 1 6 -6 h{} a6 6 0 0 1 6 6 v{} h{} z" {}/>"#,
            y + 6.,
            w - 12.,
            header - 6.,
            -w,
            paint("fill", palette.header)
        );
        let _ = write!(
            out,
            r#"<line x1="{x}" y1="{}" x2="{}" y2="{}" {}/>"#,
            y + header,
            x + w,
            y + header,
            paint("stroke", palette.border)
        );
        let middle = y + header / 2. + 4.;
        let estimate = rows(&table.name);
        let estimate_w = estimate.as_deref().map_or(0., |rows| (look.measure)(rows, 11., false) + gap);
        let _ = write!(
            out,
            r#"<text x="{}" y="{middle}" font-size="12" font-weight="600" {}>{}</text>"#,
            x + pad,
            paint("fill", palette.text),
            escape(&fit(&table.name, w - pad * 2. - estimate_w, 12., true))
        );
        if let Some(rows) = estimate {
            let _ = write!(
                out,
                r#"<text x="{}" y="{middle}" font-size="11" text-anchor="end" {}>{}</text>"#,
                x + w - pad,
                paint("fill", palette.muted),
                escape(&rows)
            );
        }
        for (row, column) in table.shown(keys_only).iter().enumerate() {
            let top = y + header + ROW_H * REM * row as f32;
            let base = top + ROW_H * REM / 2. + 4.;
            let icon = match (column.is_primary, column.is_foreign) {
                (true, _) => Some(("key", palette.key)),
                (false, true) => Some(("link", palette.link)),
                _ => None,
            };
            if let Some((id, color)) = icon {
                let _ = write!(
                    out,
                    r##"<use href="#{id}" x="{}" y="{}" width="{side}" height="{side}" color="{}"/>"##,
                    x + pad,
                    top + (ROW_H * REM - side) / 2.,
                    hex(color)
                );
            }
            let type_name = fit(&display_type(dialect, &column.data_type), 6.154 * REM, 11., false);
            let type_w = (look.measure)(&type_name, 11., false);
            let name_room = w - pad * 2. - side - gap * 2. - type_w;
            let _ = write!(
                out,
                r#"<text x="{}" y="{base}" font-size="12" {}>{}</text>"#,
                x + pad + side + gap,
                paint("fill", palette.text),
                escape(&fit(&column.name, name_room, 12., false))
            );
            let _ = write!(
                out,
                r#"<text x="{}" y="{base}" font-size="11" text-anchor="end" {}>{}</text>"#,
                x + w - pad,
                paint("fill", palette.muted),
                escape(&type_name)
            );
        }
        out.push_str("</g>");
        // GPUI draws a box's border inside it, over what it holds.
        let _ = write!(
            out,
            r#"<rect x="{}" y="{}" width="{}" height="{}" rx="5.5" fill="none" {}{}/>"#,
            x + 0.5,
            y + 0.5,
            w - 1.,
            h - 1.,
            paint("stroke", palette.border),
            if table.view { r#" stroke-dasharray="4 3""# } else { "" }
        );
    }
    out.push_str("</g></svg>\n");
    out
}

fn edges(out: &mut String, layout: &Layout, palette: &Palette) {
    let p = |(x, y): Spot| format!("{} {}", x * REM, y * REM);
    let _ = write!(out, r#"<g fill="none" {} stroke-width="1.5">"#, paint("stroke", palette.edge));
    for edge in &layout.edges {
        let path = layout.path(edge);
        let (c1, c2) = path.controls;
        let _ = write!(out, r#"<path d="M{} C{} {} {}"/>"#, p(path.from), p(c1), p(c2), p(path.to));
        for (start, end) in path.prongs.into_iter().chain([path.bar]) {
            let _ = write!(out, r#"<path d="M{} L{}"/>"#, p(start), p(end));
        }
    }
    out.push_str("</g>");
}

// Cut to `room` px with an ellipsis, as the canvas truncates.
fn fit(text: &str, room: f32, size: f32, bold: bool, measure: &dyn Fn(&str, f32, bool) -> f32) -> String {
    if measure(text, size, bold) <= room {
        return text.to_string();
    }
    let mut width = measure("…", size, bold);
    let mut cut = String::new();
    for ch in text.chars() {
        width += measure(ch.encode_utf8(&mut [0; 4]), size, bold);
        if width > room {
            break;
        }
        cut.push(ch);
    }
    cut + "…"
}

// What a Lucide file holds inside its <svg> element.
fn inner(icon: &str) -> &str {
    let start = icon.find('>').map_or(0, |ix| ix + 1);
    let end = icon.rfind("</svg>").unwrap_or(icon.len());
    icon.get(start..end).unwrap_or_default()
}

// SVG 1.1 has no #rrggbbaa, so alpha goes in its own attribute.
fn paint(attribute: &str, color: Hsla) -> String {
    let alpha = color.to_rgb().a;
    match alpha < 1. {
        true => format!(r#"{attribute}="{}" {attribute}-opacity="{alpha:.2}""#, hex(color)),
        false => format!(r#"{attribute}="{}""#, hex(color)),
    }
}

fn hex(color: Hsla) -> String {
    let Rgba { r, g, b, .. } = color.to_rgb();
    let byte = |v: f32| (v * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use barsql_core::ColumnInfo;
    use gpui_kit::hsla;

    use super::*;
    use crate::er_diagram::layout::arrange;

    fn column(name: &str, primary: bool, foreign: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "integer".into(),
            is_primary: primary,
            is_foreign: !foreign.is_empty(),
            foreign_table: foreign.into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_svg_draws_each_table_its_keys_and_their_lines() {
        let long = "a_column_name_far_too_long_to_fit_in_the_box";
        let tables = vec![
            ErTable::new("users".into(), vec![column("id", true, "")]),
            ErTable::new("posts".into(), vec![column("id", true, ""), column("author_id", false, "users")]),
            ErTable::new("notes".into(), vec![column(long, false, "")]),
        ];
        let layout = arrange(&tables, false);
        let gray = hsla(0., 0., 0.5, 1.);
        let palette = Palette {
            background: gray,
            header: gray,
            border: gray,
            text: gray,
            muted: gray,
            key: gray,
            link: gray,
            edge: gray.opacity(0.6),
        };
        let icon = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M1 1"/></svg>"#;
        let measure = |text: &str, size: f32, _: bool| text.chars().count() as f32 * size * 0.6;
        let look = Look { palette, icons: [icon, icon], measure: &measure };
        let rows = |table: &str| (table == "posts").then(|| "1.2k".to_string());
        let svg = render(&tables, &layout, false, None, &look, rows);
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>\n"));
        for text in [">users</text>", ">posts</text>", ">author_id</text>", ">1.2k</text>", ">integer</text>"] {
            assert!(svg.contains(text), "{text}");
        }
        assert!(!svg.contains(long) && svg.contains("a_column_name_far…</text>"), "cut to the box");
        assert_eq!(svg.matches(r##"href="#key""##).count(), 2);
        assert_eq!(svg.matches(r##"href="#link""##).count(), 1);
        assert_eq!(svg.matches(" C").count(), 1, "one line for one key");
        assert!(svg.contains(r#"<path d="M1 1"/>"#), "icons keep their own paths");
        assert!(svg.contains(r##"stroke="#808080" stroke-opacity="0.60""##), "lines keep their opacity");
    }

    #[test]
    fn names_are_escaped() {
        assert_eq!(escape(r#"a<b>&"c""#), "a&lt;b&gt;&amp;&quot;c&quot;");
    }
}
