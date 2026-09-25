// Kept to one test in its own binary because it sets process-wide environment variables.
use barsql_core::paths::{DATA_FOLDER_NAME, data_dir, ensure_data_dir};

fn set(value: &str) {
    // SAFETY: this binary runs a single test, so nothing else reads the environment concurrently.
    unsafe { std::env::set_var("BARSQL_DATA_DIR", value) };
}

#[test]
fn the_data_dir_honours_the_override_and_is_created() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().display().to_string();
    set(&target);
    assert_eq!(data_dir(), tmp.path());
    set(&format!("  {target}  "));
    assert_eq!(data_dir(), tmp.path(), "the override is trimmed");

    let nested = tmp.path().join("nested").join("dir");
    set(&nested.display().to_string());
    assert_eq!(ensure_data_dir().unwrap(), nested);
    assert!(nested.is_dir());

    for blank in ["", "   "] {
        set(blank);
        assert!(data_dir().ends_with(DATA_FOLDER_NAME), "{:?}", data_dir());
    }
}
