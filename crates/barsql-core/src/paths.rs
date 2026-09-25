use std::env;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DATA_FOLDER_NAME: &str = "BarSQL-data";

fn overridden() -> Option<PathBuf> {
    let dir = env::var_os("BARSQL_DATA_DIR")?.to_string_lossy().trim().to_string();
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

pub fn data_dir() -> PathBuf {
    overridden().unwrap_or_else(resolve)
}

fn resolve() -> PathBuf {
    let exe_dir = executable_dir();
    let wd = env::current_dir().ok();
    let cfg_dir = dirs::config_dir();

    let dev_mode = exe_dir.as_deref().is_some_and(is_cargo_target_dir);
    let portable = exe_dir
        .as_deref()
        .filter(|_| !dev_mode)
        .map(|exe| portable_data_dir(env::consts::OS, exe))
        .filter(|cand| is_writable_dir(cand));
    resolve_data_dir(dev_mode, wd.as_deref(), portable.as_deref(), cfg_dir.as_deref(), exe_dir.as_deref())
}

pub fn ensure_data_dir() -> io::Result<PathBuf> {
    let dir = data_dir();
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn resolve_data_dir(
    dev_mode: bool,
    wd: Option<&Path>,
    portable: Option<&Path>,
    cfg_dir: Option<&Path>,
    exe_dir: Option<&Path>,
) -> PathBuf {
    match (dev_mode, wd, portable, cfg_dir, exe_dir) {
        (true, Some(wd), ..) => wd.join(DATA_FOLDER_NAME),
        (_, _, Some(portable), ..) => portable.to_path_buf(),
        (_, _, _, Some(cfg), _) => cfg.join(DATA_FOLDER_NAME),
        (_, _, _, _, Some(exe)) => exe.join(DATA_FOLDER_NAME),
        _ => Path::new(".").join(DATA_FOLDER_NAME),
    }
}

fn portable_data_dir(os: &str, exe_dir: &Path) -> PathBuf {
    if os == "macos"
        && let Some(parent) = mac_app_bundle_parent(exe_dir)
    {
        return parent.join(DATA_FOLDER_NAME);
    }
    exe_dir.join(DATA_FOLDER_NAME)
}

fn mac_app_bundle_parent(exe_dir: &Path) -> Option<PathBuf> {
    let contents = exe_dir.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = exe_dir.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|ext| ext == "app");
    is_bundle.then(|| bundle.parent().map(Path::to_path_buf)).flatten()
}

fn is_writable_dir(dir: &Path) -> bool {
    let target = if dir.exists() {
        dir
    } else {
        match dir.parent() {
            Some(parent) => parent,
            None => return false,
        }
    };
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let probe = target.join(format!(".barsql-probe-{}-{nanos}", process::id()));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn executable_dir() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    exe.parent().map(Path::to_path_buf)
}

fn is_cargo_target_dir(dir: &Path) -> bool {
    let dir = if dir.ends_with("deps") { dir.parent().unwrap_or(dir) } else { dir };
    let profile = dir.file_name().and_then(|n| n.to_str());
    let parent = dir.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str());
    matches!(profile, Some("debug" | "release")) && parent == Some("target")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_data_dir_order() {
        let exe = Path::new("/usr/bin");
        let wd = Path::new("/work/dir");
        let portable = Path::new("/apps/BarSQL-data");
        let cfg = Path::new("/home/u/.config");
        let cases = [
            (
                "dev mode beats everything",
                true,
                Some(wd),
                Some(portable),
                Some(cfg),
                Some(exe),
                wd.join(DATA_FOLDER_NAME),
            ),
            ("writable portable wins", false, Some(wd), Some(portable), Some(cfg), Some(exe), portable.to_path_buf()),
            (
                "read-only portable falls back to config dir",
                false,
                Some(wd),
                None,
                Some(cfg),
                Some(exe),
                cfg.join(DATA_FOLDER_NAME),
            ),
            ("no config dir falls back to exe dir", false, Some(wd), None, None, Some(exe), exe.join(DATA_FOLDER_NAME)),
            (
                "nothing known falls back to ./BarSQL-data",
                false,
                None,
                None,
                None,
                None,
                Path::new(".").join(DATA_FOLDER_NAME),
            ),
        ];
        for (name, dev, wd, portable, cfg, exe, want) in cases {
            assert_eq!(resolve_data_dir(dev, wd, portable, cfg, exe), want, "{name}");
        }
    }

    #[test]
    fn portable_data_dir_per_os() {
        let cases = [
            ("windows", r"C:\Apps\BarSQL", Path::new(r"C:\Apps\BarSQL").join(DATA_FOLDER_NAME)),
            ("linux", "/opt/barsql", Path::new("/opt/barsql").join(DATA_FOLDER_NAME)),
            (
                "macos",
                "/Users/u/Desktop/BarSQL.app/Contents/MacOS",
                Path::new("/Users/u/Desktop").join(DATA_FOLDER_NAME),
            ),
            ("macos", "/Users/u/bin", Path::new("/Users/u/bin").join(DATA_FOLDER_NAME)),
        ];
        for (os, exe, want) in cases {
            assert_eq!(portable_data_dir(os, Path::new(exe)), want, "{os} {exe}");
        }
    }

    #[test]
    fn mac_app_bundle_parent_detection() {
        assert_eq!(
            mac_app_bundle_parent(Path::new("/Applications/BarSQL.app/Contents/MacOS")),
            Some(PathBuf::from("/Applications"))
        );
        assert_eq!(mac_app_bundle_parent(Path::new("/usr/local/bin")), None);
    }

    #[test]
    fn cargo_target_dirs_count_as_dev_mode() {
        assert!(is_cargo_target_dir(Path::new("/src/barsql/target/debug")));
        assert!(is_cargo_target_dir(Path::new("/src/barsql/target/release")));
        assert!(is_cargo_target_dir(Path::new("/src/barsql/target/debug/deps")));
        assert!(!is_cargo_target_dir(Path::new(r"C:\Program Files\BarSQL")));
        assert!(!is_cargo_target_dir(Path::new("/Applications/BarSQL.app/Contents/MacOS")));
    }

    #[test]
    fn writable_dir_probe() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(is_writable_dir(tmp.path()));
        assert!(is_writable_dir(&tmp.path().join("child")));
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_parent_is_not_writable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let ro = tmp.path().join("ro");
        fs::create_dir(&ro).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o500)).unwrap();
        // Root ignores directory permissions, so there is nothing to observe.
        if fs::write(ro.join("probe"), b"").is_ok() {
            return;
        }
        let writable = is_writable_dir(&ro.join("child"));
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!writable);
    }
}
