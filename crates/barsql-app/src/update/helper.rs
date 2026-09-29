use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::install::STAGING_PREFIX;
use super::{Install, Kind, admin, appimage_file};

// Restart & Apply relaunches this binary with these set. It waits for the app to exit, puts the new build
// in place and launches it. When that fails it launches the old build again.
const ENV_MODE: &str = "BARSQL_UPDATER_HELPER";
const ENV_TARGET: &str = "BARSQL_UPDATER_TARGET";
const ENV_NEW: &str = "BARSQL_UPDATER_NEW";
const ENV_PACKAGE: &str = "BARSQL_UPDATER_PACKAGE";
const ENV_VERSION: &str = "BARSQL_UPDATER_VERSION";
const ENV_PROMPT: &str = "BARSQL_UPDATER_PROMPT";
const ENV_PID: &str = "BARSQL_UPDATER_PID";
const ENV_LOG: &str = "BARSQL_UPDATER_LOG";
// When the target's folder isn't writable, the helper runs `BarSQL --barsql-update-swap <target> <new>
// <version> <log>` behind the OS admin prompt. The prompt passes no environment, hence the arguments.
const SWAP_FLAG: &str = "--barsql-update-swap";
const PARENT_TIMEOUT: Duration = Duration::from_secs(30);
const SWAP_ATTEMPTS: usize = 20;
// In the data folder, started afresh by each update.
const UPDATE_LOG: &str = "update.log";
// The helper writes this into the staging folder when it starts.
const CLAIM: &str = ".helper";
// How long after anything last touched it a staging folder whose app is gone counts as abandoned.
const STALE_AFTER: Duration = Duration::from_secs(60 * 60);
#[cfg(windows)]
const ASIDE_ATTEMPTS: usize = 20;

// On macOS the whole .app bundle, elsewhere the binary itself.
pub fn bundle_target(exe: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        let mut path = PathBuf::new();
        for component in exe.components() {
            path.push(component);
            if path.extension().is_some_and(|ext| ext == "app") {
                return path;
            }
        }
    }
    exe.to_path_buf()
}

pub fn spawn_helper(install: &Install, staged: &Path, version: &str, prompt: &str) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    // The app's AppImage mount can go away with the app, so the helper starts from the AppImage file.
    let program = if install.kind == Kind::AppImage { install.target.clone() } else { exe };
    let log = barsql_core::paths::data_dir().join(UPDATE_LOG);
    let mut command = Command::new(program);
    command
        .env(ENV_MODE, "1")
        .env(ENV_TARGET, &install.target)
        .env(ENV_NEW, staged)
        .env(ENV_VERSION, version)
        .env(ENV_PROMPT, prompt)
        .env(ENV_PID, std::process::id().to_string())
        .env(ENV_LOG, log)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(package) = install.kind.package() {
        command.env(ENV_PACKAGE, package);
    }
    detach(&mut command);
    command.spawn().map(|_| ())
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

// main() calls this first. As the helper or its admin half it does the job and returns the exit code.
pub fn helper_mode() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == SWAP_FLAG) {
        let [_, target, new, version, log] = args.as_slice() else { return Some(2) };
        return Some(admin_swap(Path::new(target), Path::new(new), &version.to_string_lossy(), Path::new(log)));
    }
    if std::env::var(ENV_MODE).ok().as_deref() != Some("1") {
        return None;
    }
    let (Some(target), Some(new)) = (std::env::var_os(ENV_TARGET), std::env::var_os(ENV_NEW)) else {
        return Some(2);
    };
    let var = |key: &str| std::env::var(key).unwrap_or_default();
    let job = Job {
        target: target.into(),
        new: new.into(),
        package: Kind::from_package(&var(ENV_PACKAGE)),
        version: var(ENV_VERSION),
        prompt: var(ENV_PROMPT),
        parent: var(ENV_PID).parse().unwrap_or(0),
        log: std::env::var_os(ENV_LOG).map(PathBuf::from),
    };
    for key in [ENV_MODE, ENV_TARGET, ENV_NEW, ENV_PACKAGE, ENV_VERSION, ENV_PROMPT, ENV_PID, ENV_LOG] {
        // SAFETY: the helper is single-threaded at this point, before anything reads the environment.
        unsafe { std::env::remove_var(key) };
    }
    Some(run_helper(&job, &Native))
}

pub(crate) struct Job {
    pub target: PathBuf,
    pub new: PathBuf,
    pub package: Option<Kind>,
    pub version: String,
    pub prompt: String,
    pub parent: u32,
    pub log: Option<PathBuf>,
}

// What the helper needs from the OS, so tests can stand in for it.
pub(crate) trait Host {
    fn exited(&self, pid: u32, timeout: Duration) -> bool;
    fn launch(&self, target: &Path) -> io::Result<()>;
    fn as_admin(&self, program: &Path, args: &[OsString], prompt: &str) -> io::Result<i32>;
}

struct Native;

impl Host for Native {
    fn exited(&self, pid: u32, timeout: Duration) -> bool {
        wait_for_exit(pid, timeout)
    }

    fn launch(&self, target: &Path) -> io::Result<()> {
        launch(target)
    }

    fn as_admin(&self, program: &Path, args: &[OsString], prompt: &str) -> io::Result<i32> {
        admin::run(program, args, prompt)
    }
}

struct Log(Option<fs::File>);

impl Log {
    // The helper empties the log, then appends, so the admin half's lines in between survive.
    fn create(path: Option<&Path>) -> Self {
        Self(path.and_then(|path| {
            File::create(path).ok()?;
            OpenOptions::new().append(true).open(path).ok()
        }))
    }

    // The admin half adds to the helper's log and never creates one, which as root would be root's.
    fn append(path: &Path) -> Self {
        Self(OpenOptions::new().append(true).open(path).ok())
    }

    fn line(&mut self, text: impl AsRef<str>) {
        if let Some(file) = &mut self.0 {
            let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let _ = writeln!(file, "{secs} {}", text.as_ref());
        }
    }
}

// Exit codes: 10 the target is missing, 11 the new build is missing, 12 no backup, 13 the swap failed and
// the backup is back, 14 the restore failed too, 15/16 the same after a failed launch, 17 the app didn't
// exit, 18 the admin step was refused or failed. After 11, 12, 13 and 18 the old build launches again.
pub(crate) fn run_helper(job: &Job, host: &impl Host) -> i32 {
    claim_staging(&job.new);
    let mut log = Log::create(job.log.as_deref());
    log.line(format!("helper start: target={} new={} pid={}", job.target.display(), job.new.display(), job.parent));
    if fs::symlink_metadata(&job.target).is_err() {
        log.line("target missing");
        return 10;
    }
    if job.parent > 0 && !host.exited(job.parent, PARENT_TIMEOUT) {
        log.line("the app did not exit in time, nothing changed");
        return 17;
    }
    let backup = backup_path(&job.target);
    let (code, as_admin) = put_in_place(job, &backup, host, &mut log);
    let code = match code {
        0 => match host.launch(&job.target) {
            Ok(()) => 0,
            Err(error) if as_admin => {
                log.line(format!("launch failed: {error}, and the admin step kept no backup"));
                16
            }
            Err(error) => {
                log.line(format!("launch failed: {error}, restoring the backup"));
                match restore(&backup, &job.target) {
                    Ok(()) => {
                        let _ = host.launch(&job.target);
                        15
                    }
                    Err(error) => {
                        log.line(format!("restore failed: {error}"));
                        16
                    }
                }
            }
        },
        14 => 14,
        code => {
            log.line("launching the old build again");
            let _ = host.launch(&job.target);
            code
        }
    };
    // After a failed restore the backup is the only good copy.
    if !matches!(code, 14 | 16) {
        let _ = remove_any(&backup);
    }
    remove_staging(&job.new);
    log.line(format!("helper done: {code}"));
    code
}

// Returns the code and whether the admin prompt was involved.
fn put_in_place(job: &Job, backup: &Path, host: &impl Host, log: &mut Log) -> (i32, bool) {
    if fs::symlink_metadata(&job.new).is_err() {
        log.line("new missing");
        return (11, false);
    }
    if let Some(kind) = job.package {
        let (program, args) = package_install(kind, &job.new);
        let code = match host.as_admin(Path::new(program), &args, &job.prompt) {
            Ok(0) => 0,
            result => {
                log.line(format!("{program} failed: {result:?}"));
                18
            }
        };
        return (code, true);
    }
    match writable(&job.target) {
        Ok(()) => (swap(&job.target, &job.new, backup, false, log), false),
        Err(error) if error.kind() == io::ErrorKind::ReadOnlyFilesystem => {
            log.line(format!("the app is on a read-only volume: {error}"));
            (12, false)
        }
        Err(error) => {
            log.line(format!("asking for admin rights: {error}"));
            let log_path = job.log.clone().unwrap_or_default();
            let args = [OsStr::new(SWAP_FLAG), job.target.as_os_str(), job.new.as_os_str(), OsStr::new(&job.version)];
            let args: Vec<OsString> = args.into_iter().chain([log_path.as_os_str()]).map(OsString::from).collect();
            let code = match host.as_admin(&self_program(), &args, &job.prompt) {
                // The admin half returns the swap's own codes.
                Ok(code @ (0 | 12 | 13 | 14)) => code,
                result => {
                    log.line(format!("the admin swap failed: {result:?}"));
                    18
                }
            };
            (code, true)
        }
    }
}

// Runs with admin rights. The helper can't clean up a protected folder, so the backup goes once the swap works.
pub(crate) fn admin_swap(target: &Path, new: &Path, version: &str, log: &Path) -> i32 {
    let mut log = Log::append(log);
    log.line("admin swap start");
    let backup = backup_path(target);
    let code = swap(target, new, &backup, true, &mut log);
    if code == 0 {
        let _ = remove_any(&backup);
        register_version(target, version, &mut log);
        delete_asides_at_reboot(target, &mut log);
    }
    code
}

// Backs up the target, puts the new build in its place and restores the backup when that fails. The admin
// half copies instead of moving, so the new build gets the protected folder's owner and access rules.
fn swap(target: &Path, new: &Path, backup: &Path, copy: bool, log: &mut Log) -> i32 {
    let _ = remove_any(backup);
    if let Err(error) = copy_any(target, backup) {
        log.line(format!("backup failed: {error}"));
        let _ = remove_any(backup);
        return 12;
    }
    let mode = fs::metadata(target).ok().map(|m| m.permissions());
    for attempt in 1..=SWAP_ATTEMPTS {
        match replace_target(target, new, copy) {
            Ok(()) => {
                log.line(format!("swapped on attempt {attempt}"));
                if let Some(mode) = mode.filter(|_| target.is_file()) {
                    let _ = fs::set_permissions(target, mode);
                }
                return 0;
            }
            Err(error) => {
                log.line(format!("replace (attempt {attempt}): {error}"));
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    match restore(backup, target) {
        Ok(()) => 13,
        Err(error) => {
            log.line(format!("restore failed: {error}"));
            14
        }
    }
}

fn package_install(kind: Kind, file: &Path) -> (&'static str, Vec<OsString>) {
    let (program, flags): (&str, &[&str]) = match kind {
        Kind::Rpm => ("/usr/bin/rpm", &["-U"]),
        Kind::Pacman => ("/usr/bin/pacman", &["-U", "--noconfirm"]),
        _ => ("/usr/bin/dpkg", &["-i"]),
    };
    (program, flags.iter().map(OsString::from).chain([file.as_os_str().to_owned()]).collect())
}

// Root can't read another user's AppImage mount, so the admin half runs the AppImage file itself. Not
// canonicalized, since the shell's runas verb may not take a \\?\ path on Windows.
fn self_program() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    appimage_file(&exe).unwrap_or(exe)
}

fn backup_path(target: &Path) -> PathBuf {
    let mut path = target.as_os_str().to_owned();
    path.push(".bak");
    path.into()
}

// The folder download_and_stage made for the new build.
fn staging_of(new: &Path) -> Option<&Path> {
    new.parent().filter(|dir| dir.file_name().is_some_and(|name| name.to_string_lossy().starts_with(STAGING_PREFIX)))
}

fn remove_staging(new: &Path) {
    if let Some(staging) = staging_of(new) {
        let _ = fs::remove_dir_all(staging);
    }
}

// The app is gone while the helper works, so the claim is what keeps a BarSQL started meanwhile from
// sweeping the new build away.
fn claim_staging(new: &Path) {
    if let Some(staging) = staging_of(new) {
        let _ = fs::write(staging.join(CLAIM), std::process::id().to_string());
    }
}

// Swapping removes the target and puts a new one in its folder, and a bundle's own files go too.
fn writable(target: &Path) -> io::Result<()> {
    let dir = target.parent().ok_or_else(|| io::Error::other("the target has no folder"))?;
    check_write(dir)?;
    if target.is_dir() {
        check_write(target)?;
    }
    Ok(())
}

#[cfg(unix)]
fn check_write(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: a NUL-terminated path that outlives the call.
    if unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

// Windows grants access through ACLs, so try creating a file instead.
#[cfg(windows)]
fn check_write(dir: &Path) -> io::Result<()> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let probe = dir.join(format!(".barsql-probe-{}-{nanos}", std::process::id()));
    OpenOptions::new().write(true).create_new(true).open(&probe)?;
    fs::remove_file(&probe)
}

// The installer puts uninstall.exe next to BarSQL.exe and lists the version under Installed apps.
#[cfg(windows)]
fn register_version(target: &Path, version: &str, log: &mut Log) {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, REG_SZ, RegSetKeyValueW};
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Bare7aBarSQL";
    if version.is_empty() || !target.with_file_name("uninstall.exe").is_file() {
        return;
    }
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (key, name, value) = (wide(UNINSTALL_KEY), wide("DisplayVersion"), wide(version));
    // SAFETY: NUL-terminated UTF-16 buffers that outlive the call, with the value's size in bytes.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * 2) as u32,
        )
    };
    if status != 0 {
        log.line(format!("DisplayVersion not updated: {}", io::Error::from_raw_os_error(status as i32)));
    }
}

#[cfg(not(windows))]
fn register_version(_: &Path, _: &str, _: &mut Log) {}

fn restore(backup: &Path, target: &Path) -> io::Result<()> {
    let _ = remove_any(target);
    rename_or_copy(backup, target)
}

fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

// Rename fails across volumes, so fall back to a copy.
fn rename_or_copy(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_any(from, to)?;
            let _ = remove_any(from);
            Ok(())
        }
    }
}

#[cfg(not(windows))]
fn replace_target(target: &Path, new: &Path, copy: bool) -> io::Result<()> {
    remove_any(target)?;
    if copy { copy_any(new, target) } else { rename_or_copy(new, target) }
}

// Windows can rename a running .exe but not delete it, so move it aside first.
#[cfg(windows)]
fn replace_target(target: &Path, new: &Path, copy: bool) -> io::Result<()> {
    let put = || if copy { copy_any(new, target) } else { rename_or_copy(new, target) };
    if remove_any(target).is_ok() {
        let _ = sweep_asides(target);
        return put();
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let aside = PathBuf::from(format!("{}.old.{nanos}", target.display()));
    fs::rename(target, &aside)?;
    if let Err(error) = put() {
        let _ = fs::rename(&aside, target);
        return Err(error);
    }
    let _ = sweep_asides(target);
    Ok(())
}

// Returns the asides that are still there.
#[cfg(windows)]
fn sweep_asides(target: &Path) -> Vec<PathBuf> {
    asides(target).into_iter().filter(|aside| fs::remove_file(aside).is_err()).collect()
}

#[cfg(windows)]
fn asides(target: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else { return Vec::new() };
    let prefix = format!("{}.old.", name.to_string_lossy());
    let entries = fs::read_dir(dir).into_iter().flatten().flatten();
    entries.filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix)).map(|entry| entry.path()).collect()
}

// Clears what earlier updates left behind, off the main thread. main() calls this once the app is the one
// running.
pub fn sweep_update_leftovers() {
    std::thread::spawn(|| {
        sweep_staging(&std::env::temp_dir(), SystemTime::now(), alive);
        #[cfg(windows)]
        sweep_own_asides();
    });
}

// The helper runs from the target, so its aside stays locked until it exits. A protected folder needs admin
// rights to delete from, so there the admin half has already handed the aside to Windows for the next boot.
#[cfg(windows)]
fn sweep_own_asides() {
    let Ok(exe) = std::env::current_exe() else { return };
    if writable(&exe).is_err() {
        return;
    }
    for _ in 0..ASIDE_ATTEMPTS {
        if sweep_asides(&exe).is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

// Removes the staging folders of apps that quit or crashed without applying their download, and the logs
// older versions left beside them, once their app is gone and nothing has touched them for STALE_AFTER.
pub(crate) fn sweep_staging(dir: &Path, now: SystemTime, alive: impl Fn(u32) -> bool) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some(owner) = leftover_owner(&entry.file_name().to_string_lossy()) else { continue };
        let path = entry.path();
        let stale =
            last_touched(&path).is_some_and(|touched| now.duration_since(touched).is_ok_and(|age| age >= STALE_AFTER));
        if stale && !alive(owner) {
            let _ = remove_any(&path);
        }
    }
}

// The app's pid, from barsql-update-<pid>-<nanos> for a staging folder or barsql-update-<pid>.log for an old log.
fn leftover_owner(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(STAGING_PREFIX)?;
    let (pid, tail) = rest.split_at(rest.find(|c: char| !c.is_ascii_digit())?);
    let nanos = tail.strip_prefix('-').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if nanos || tail == ".log" { pid.parse().ok() } else { None }
}

// The newest change to the entry or anything directly in it, such as the helper's claim. None when any of
// that can't be read, which keeps the entry.
fn last_touched(path: &Path) -> Option<SystemTime> {
    let meta = fs::symlink_metadata(path).ok()?;
    let mut newest = meta.modified().ok()?;
    if meta.is_dir() {
        for entry in fs::read_dir(path).ok()? {
            newest = newest.max(entry.ok()?.metadata().ok()?.modified().ok()?);
        }
    }
    Some(newest)
}

#[cfg(windows)]
fn delete_asides_at_reboot(target: &Path, log: &mut Log) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
    for aside in sweep_asides(target) {
        let wide: Vec<u16> = aside.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: a NUL-terminated UTF-16 path that outlives the call. A null new name means delete.
        if unsafe { MoveFileExW(wide.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT) } == 0 {
            log.line(format!("{} not queued for deletion: {}", aside.display(), io::Error::last_os_error()));
        }
    }
}

#[cfg(not(windows))]
fn delete_asides_at_reboot(_: &Path, _: &mut Log) {}

// Recursive, keeping symlinks and permissions.
pub(crate) fn copy_any(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        return copy_link(from, to);
    }
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        fs::set_permissions(to, meta.permissions())?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_any(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    fs::copy(from, to)?;
    Ok(())
}

#[cfg(unix)]
fn copy_link(from: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(from)?, to)
}

#[cfg(windows)]
fn copy_link(from: &Path, to: &Path) -> io::Result<()> {
    fs::copy(from, to).map(|_| ())
}

fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: the handle is checked before use and closed afterwards.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}

fn launch(target: &Path) -> io::Result<()> {
    let mut command = if cfg!(target_os = "macos") && target.extension().is_some_and(|ext| ext == "app") {
        let mut open = Command::new("open");
        open.arg("-n").arg(target);
        open
    } else {
        Command::new(target)
    };
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    detach(&mut command);
    command.spawn().map(|_| ())
}
