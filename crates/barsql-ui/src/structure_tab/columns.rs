use barsql_core::Identity;
use barsql_sql::structure::Draft;
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{cell, line};
use crate::form;
use crate::i18n::{t, t_with};
use crate::tokens::TEXT_XS;

pub(super) fn identity_label(identity: Identity, cx: &App) -> Option<SharedString> {
    Some(match identity {
        Identity::None => return None,
        Identity::ByDefault => t(cx, "structure.identity.byDefault"),
        Identity::Always => t(cx, "structure.identity.always"),
        Identity::AutoIncrement => t(cx, "structure.identity.autoIncrement"),
        Identity::Seeded { seed, increment } => {
            let (seed, increment) = (seed.to_string(), increment.to_string());
            t_with(cx, "structure.identity.seeded", &[("seed", &seed), ("increment", &increment)])
        }
    })
}

pub(super) fn read_only(draft: &Draft, cx: &App) -> impl IntoElement {
    let theme = cx.theme().clone();
    let heading = |key: &str, width: f32| cell(&t(cx, key), width);
    let header = line(|| "structure:columns-header".into(), cx)
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .child(heading("structure.name", 14.))
        .child(heading("structure.type", 11.))
        .child(heading("structure.nullable", 6.))
        .child(heading("structure.default", 11.))
        .child(div().flex_1().child(t(cx, "structure.comment")));
    let rows = draft.columns.iter().filter(|row| !row.dropped).map(|row| {
        let column = &row.column;
        let name = column.name.clone();
        let default = match column.generated.as_str() {
            "" => column.default.clone(),
            generated => format!("{} {generated}", t(cx, "structure.generated")),
        };
        line(move || format!("structure:column:{name}"), cx)
            .child(
                cell(&column.name, 14.)
                    .when(row.primary, |cell| cell.child(form::badge(t(cx, "sidebar.badge.pk"), theme.warning).ml_1())),
            )
            .child(cell(&column.data_type, 11.))
            .child(cell(if column.nullable { "NULL" } else { "NOT NULL" }, 6.))
            .child(cell(&default, 11.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(theme.muted_foreground)
                    .children(identity_label(column.identity, cx).map(|label| div().child(label)))
                    .child(column.comment.clone()),
            )
    });
    v_flex().child(header).children(rows)
}
