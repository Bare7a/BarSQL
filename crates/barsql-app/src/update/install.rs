use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::Release;

const MAX_ENTRIES: usize = 50_000;
const MAX_TOTAL_SIZE: u64 = 2 << 30;
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Check,
    Download,
    Verify,
    Install,
}

impl Stage {
    pub fn key(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Download => "download",
            Self::Verify => "verify",
            Self::Install => "install",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallError {
    pub stage: Stage,
    pub message: String,
}

impl InstallError {
    fn new(stage: Stage, message: impl ToString) -> Self {
        Self { stage, message: message.to_string() }
    }
}

// `path` is the app bundle or binary, verified and ready for the swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub path: PathBuf,
    pub dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InstallEvent {
    Downloading { written: u64, total: u64, rate: u64 },
    Verifying,
    Installing,
}

fn staging_dir() -> io::Result<PathBuf> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("barsql-update-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// Downloads to a fresh staging folder, checks the SHA256SUMS digest, then unpacks. An asset without a
// digest is refused.
pub fn download_and_stage(
    release: &Release,
    stop: &AtomicBool,
    mut on_event: impl FnMut(InstallEvent),
) -> Result<Staged, InstallError> {
    let dir = staging_dir().map_err(|e| InstallError::new(Stage::Download, e))?;
    let result = stage_into(&dir, release, stop, &mut on_event);
    if result.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    result
}

fn stage_into(
    dir: &Path,
    release: &Release,
    stop: &AtomicBool,
    on_event: &mut impl FnMut(InstallEvent),
) -> Result<Staged, InstallError> {
    let download = |e: String| InstallError::new(Stage::Download, e);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .user_agent(format!("BarSQL/{}", crate::VERSION))
        .build()
        .into();
    let mut response = agent
        .get(&release.asset.url)
        .header("Accept", "application/octet-stream")
        .call()
        .map_err(|e| download(format!("download: {e}")))?;
    let length = response.body().content_length().unwrap_or(0);
    let total = if release.asset.size > 0 { release.asset.size } else { length };
    let mut reader = response.body_mut().with_config().limit(MAX_TOTAL_SIZE).reader();
    let artifact = dir.join(".artifact");
    let mut file = File::create(&artifact).map_err(|e| download(e.to_string()))?;
    let mut hasher = Sha256::new();
    let (mut written, mut buffer) = (0u64, vec![0u8; 64 * 1024]);
    let (started, mut reported) = (Instant::now(), Instant::now() - PROGRESS_EVERY);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(download("cancelled".into()));
        }
        let n = reader.read(&mut buffer).map_err(|e| download(e.to_string()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n]).map_err(|e| download(e.to_string()))?;
        hasher.update(&buffer[..n]);
        written += n as u64;
        if reported.elapsed() >= PROGRESS_EVERY {
            reported = Instant::now();
            let seconds = started.elapsed().as_secs_f64().max(0.001);
            on_event(InstallEvent::Downloading { written, total, rate: (written as f64 / seconds) as u64 });
        }
    }
    file.sync_all().map_err(|e| download(e.to_string()))?;
    drop(file);
    let seconds = started.elapsed().as_secs_f64().max(0.001);
    on_event(InstallEvent::Downloading { written, total: total.max(written), rate: (written as f64 / seconds) as u64 });

    on_event(InstallEvent::Verifying);
    let Some(expected) = &release.digest else {
        return Err(InstallError::new(
            Stage::Verify,
            format!("the release lists no SHA-256 for {}", release.asset.name),
        ));
    };
    let actual = hasher.finalize();
    if actual.as_slice() != expected.as_slice() {
        return Err(InstallError::new(
            Stage::Verify,
            format!("checksum mismatch: expected {}, got {}", hex::encode(expected), hex::encode(actual)),
        ));
    }

    on_event(InstallEvent::Installing);
    let install = |e: String| InstallError::new(Stage::Install, e);
    let file_name = Path::new(&release.asset.name)
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| install(format!("bad asset name {}", release.asset.name)))?;
    let named = dir.join(file_name);
    fs::rename(&artifact, &named).map_err(|e| install(e.to_string()))?;
    let path = extract_single(&named).map_err(install)?;
    // A bare binary or an AppImage arrives without its executable bit.
    if path == named {
        set_mode(&path, 0o755).map_err(|e| install(e.to_string()))?;
    }
    Ok(Staged { path, dir: dir.to_path_buf() })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Archive {
    None,
    Zip,
    TarGz,
}

fn archive_kind(path: &Path) -> Archive {
    let lower = path.to_string_lossy().to_lowercase();
    if lower.ends_with(".zip") {
        Archive::Zip
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Archive::TarGz
    } else {
        Archive::None
    }
}

// Replaces the archive with its single top-level entry. Anything that isn't an archive is used as is.
pub fn extract_single(archive: &Path) -> Result<PathBuf, String> {
    let kind = archive_kind(archive);
    if kind == Archive::None {
        return Ok(archive.to_path_buf());
    }
    let parent = archive.parent().ok_or("the archive has no folder")?;
    let scratch = parent.join(".payload");
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let result = (|| {
        match kind {
            Archive::Zip => extract_zip(archive, &scratch)?,
            Archive::TarGz => extract_tar_gz(archive, &scratch)?,
            Archive::None => unreachable!(),
        }
        let entries: Vec<_> = fs::read_dir(&scratch).map_err(|e| e.to_string())?.flatten().collect();
        if entries.len() != 1 {
            return Err(format!("the archive must hold exactly one top-level entry, it holds {}", entries.len()));
        }
        fs::remove_file(archive).map_err(|e| e.to_string())?;
        let target = parent.join(entries[0].file_name());
        fs::rename(entries[0].path(), &target).map_err(|e| e.to_string())?;
        Ok(target)
    })();
    let _ = fs::remove_dir_all(&scratch);
    result
}

// An entry may not leave the extraction root.
fn safe_join(root: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.replace('\\', "/");
    let mut target = root.to_path_buf();
    for component in Path::new(&name).components() {
        match component {
            Component::Normal(part) => target.push(part),
            Component::CurDir => {}
            _ => return Err(format!("archive entry escapes the folder: {name}")),
        }
    }
    Ok(target)
}

fn check_link(link: &str, target: &Path, root: &Path) -> Result<(), String> {
    if link.starts_with('/') || Path::new(link).is_absolute() {
        return Err(format!("archive link points outside: {link}"));
    }
    let mut resolved = target.parent().unwrap_or(root).to_path_buf();
    for component in Path::new(link).components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::ParentDir => {
                if !resolved.pop() || !resolved.starts_with(root) {
                    return Err(format!("archive link escapes the folder: {link}"));
                }
            }
            Component::CurDir => {}
            _ => return Err(format!("archive link points outside: {link}")),
        }
    }
    if resolved.starts_with(root) { Ok(()) } else { Err(format!("archive link escapes the folder: {link}")) }
}

fn file_mode(mode: u32) -> u32 {
    if mode & 0o777 == 0 { 0o644 } else { mode & 0o777 }
}

fn dir_mode(mode: u32) -> u32 {
    if mode & 0o777 == 0 { 0o755 } else { mode & 0o777 }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn make_link(link: &str, target: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(link, target)
}

#[cfg(windows)]
fn make_link(link: &str, target: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(link, target)
}

fn write_link(link: &str, target: &Path, root: &Path) -> Result<(), String> {
    check_link(link, target, root)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let _ = fs::remove_file(target);
    make_link(link, target).map_err(|e| e.to_string())
}

fn copy_capped(reader: &mut impl Read, target: &Path, mode: u32, written: &mut u64) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut out = File::create(target).map_err(|e| e.to_string())?;
    let copied = io::copy(&mut reader.take(MAX_TOTAL_SIZE - *written + 1), &mut out).map_err(|e| e.to_string())?;
    *written += copied;
    if *written > MAX_TOTAL_SIZE {
        return Err(format!("the archive unpacks to more than {MAX_TOTAL_SIZE} bytes"));
    }
    drop(out);
    set_mode(target, file_mode(mode)).map_err(|e| e.to_string())
}

fn extract_zip(archive: &Path, root: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("zip: {e}"))?;
    if zip.len() > MAX_ENTRIES {
        return Err(format!("the zip has {} entries (at most {MAX_ENTRIES})", zip.len()));
    }
    let (mut links, mut written) = (Vec::new(), 0u64);
    for ix in 0..zip.len() {
        let mut entry = zip.by_index(ix).map_err(|e| format!("zip: {e}"))?;
        let target = safe_join(root, entry.name())?;
        let mode = entry.unix_mode().unwrap_or(0);
        if entry.is_symlink() {
            let mut link = String::new();
            entry.by_ref().take(4096).read_to_string(&mut link).map_err(|e| e.to_string())?;
            check_link(&link, &target, root)?;
            links.push((link, target));
        } else if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            set_mode(&target, dir_mode(mode)).map_err(|e| e.to_string())?;
        } else {
            copy_capped(&mut entry, &target, mode, &mut written)?;
        }
    }
    for (link, target) in links {
        write_link(&link, &target, root)?;
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, root: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let (mut links, mut written, mut count) = (Vec::new(), 0u64, 0usize);
    for entry in tar.entries().map_err(|e| format!("tar: {e}"))? {
        let mut entry = entry.map_err(|e| format!("tar: {e}"))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(format!("the tar has more than {MAX_ENTRIES} entries"));
        }
        let name = entry.path().map_err(|e| format!("tar: {e}"))?.to_string_lossy().into_owned();
        let target = safe_join(root, &name)?;
        let mode = entry.header().mode().unwrap_or(0);
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                set_mode(&target, dir_mode(mode)).map_err(|e| e.to_string())?;
            }
            tar::EntryType::Symlink => {
                let link = entry
                    .link_name()
                    .map_err(|e| format!("tar: {e}"))?
                    .map(|link| link.to_string_lossy().into_owned())
                    .unwrap_or_default();
                check_link(&link, &target, root)?;
                links.push((link, target));
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                copy_capped(&mut entry, &target, mode, &mut written)?;
            }
            _ => {}
        }
    }
    for (link, target) in links {
        write_link(&link, &target, root)?;
    }
    Ok(())
}
