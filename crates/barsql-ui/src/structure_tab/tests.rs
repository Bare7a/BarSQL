use barsql_app::{EditorTab, StructureRef};
use gpui_kit::TestAppContext;

use super::StructureTab;
use crate::test_support::{Env, in_root};

fn stored(env: &Env, table: &str) -> EditorTab {
    EditorTab {
        id: "structure-1".into(),
        connection_id: env.connection.id.clone(),
        title: table.into(),
        color: env.connection.color.clone(),
        structure: Some(StructureRef { schema: "main".into(), table: table.into() }),
        ..Default::default()
    }
}

#[gpui_kit::test]
fn a_structure_tab_restores_from_the_session(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, connection) = (stored(&env, "things"), env.connection.clone());
    let (view, cx) = in_root(cx, |window, cx| StructureTab::new(tab.clone(), connection, window, cx));
    assert_eq!(cx.update(|_, cx| view.read(cx).stored(cx)), tab);
    assert!(cx.update(|_, cx| view.read(cx).persisted()));
}

#[gpui_kit::test]
fn a_new_table_tab_is_not_stored(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, connection) = (stored(&env, ""), env.connection.clone());
    let (view, cx) = in_root(cx, |window, cx| StructureTab::new(tab, connection, window, cx));
    assert_eq!(cx.update(|_, cx| view.read(cx).stored(cx)).structure, None);
    assert!(!cx.update(|_, cx| view.read(cx).persisted()));
}
