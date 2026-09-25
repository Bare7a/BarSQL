use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

// An unparseable file is moved to a .corrupt-* sibling so the next write can't destroy it. Other read
// errors are returned so the caller can refuse to overwrite the file later.
pub fn load_json_file<T: DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(err) => return Err(err),
    };
    match serde_json::from_slice::<Option<T>>(&data) {
        Ok(value) => Ok(value.unwrap_or_default()),
        Err(_) => {
            backup_corrupt_file(path);
            Ok(T::default())
        }
    }
}

pub fn save_json_file<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let data = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_file_atomic(path, &data)
}

// Write-then-rename, so a crash mid-write never leaves a truncated file.
pub fn write_file_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let file_name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let tmp_path = dir.join(format!("{file_name}.tmp-{}", unix_nanos()));
    let result = (|| {
        let mut tmp = File::create_new(&tmp_path)?;
        tmp.write_all(data)?;
        tmp.sync_all()?;
        drop(tmp);
        restrict_permissions(&tmp_path)?;
        fs::rename(&tmp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result?;
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_: &Path) -> io::Result<()> {
    Ok(())
}

fn backup_corrupt_file(path: &Path) {
    let mut backup = path.as_os_str().to_owned();
    backup.push(format!(".corrupt-{}", unix_nanos()));
    let _ = fs::rename(path, backup);
}

fn unix_nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_and_overwrites_without_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("out.json");
        write_file_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
        write_file_atomic(&path, b"new content").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new content");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn atomic_write_fails_on_a_missing_directory() {
        assert!(write_file_atomic(Path::new("/no/such/dir/out.json"), b"x").is_err());
    }

    #[test]
    fn null_and_missing_files_load_as_default() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        assert_eq!(load_json_file::<Vec<String>>(&path).unwrap(), Vec::<String>::new());
        fs::write(&path, "null").unwrap();
        assert_eq!(load_json_file::<Vec<String>>(&path).unwrap(), Vec::<String>::new());
        assert!(path.exists(), "a literal null is valid, not corrupt");
    }
}
