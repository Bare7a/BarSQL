use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use barsql_core::QueryError;
use rusqlite::{Connection, LoadExtensionGuard};

use super::lite_error;

// SQLite reports most mistakes as a missing file, so the common ones are checked first. Returns each function an
// extension added, with the extension's name.
pub(super) fn load(conn: &Connection, files: &[PathBuf]) -> Result<Vec<(String, String)>, QueryError> {
    let mut added = Vec::new();
    if files.is_empty() {
        return Ok(added);
    }
    let mut known = function_names(conn)?;
    for path in files {
        let shown = path.display().to_string();
        let fail = |reason: String| QueryError::extension(&shown, reason);
        let file = resolve(path).ok_or_else(|| fail("file not found".into()))?;
        if let Some(reason) = mismatch(&file) {
            return Err(fail(reason));
        }
        #[cfg(unix)]
        let _library = Library::open(&file).map_err(|reason| fail(with_quarantine_hint(&file, reason)))?;
        // SAFETY: the user chose this library. Loading is on only for this call, so SQL can't load anything later.
        let loaded = unsafe {
            let _guard = LoadExtensionGuard::new(conn).map_err(|err| lite_error(&err))?;
            conn.load_extension(&file, None::<&str>)
        };
        if let Err(err) = loaded {
            return Err(fail(load_failure(&file, &err)));
        }
        let source = file.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
        for name in function_names(conn)? {
            if known.insert(name.clone()) {
                added.push((name, source.clone()));
            }
        }
    }
    Ok(added)
}

fn function_names(conn: &Connection) -> Result<HashSet<String>, QueryError> {
    let mut statement =
        conn.prepare("SELECT DISTINCT lower(name) FROM pragma_function_list").map_err(|err| lite_error(&err))?;
    statement.query_map([], |row| row.get(0)).and_then(Iterator::collect).map_err(|err| lite_error(&err))
}

// cr-sqlite keeps statements open until finalized, which stops the connection from closing.
pub(super) fn finalize(conn: &Connection) {
    let _ = conn.query_row("SELECT crsql_finalize()", [], |_| Ok(()));
}

// Like SQLite, tries the platform's suffix too, so one setting can serve every OS.
fn resolve(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    let mut suffixed = path.as_os_str().to_owned();
    suffixed.push(".");
    suffixed.push(std::env::consts::DLL_EXTENSION);
    Some(PathBuf::from(suffixed)).filter(|path| path.is_file())
}

fn mismatch(file: &Path) -> Option<String> {
    let mut header = Vec::new();
    match std::fs::File::open(file).and_then(|file| file.take(4096).read_to_end(&mut header)) {
        Ok(_) => mismatched(&header),
        Err(err) => Some(err.to_string()),
    }
}

// A CPU the table doesn't know is left for the loader to judge.
fn mismatched(header: &[u8]) -> Option<String> {
    let Some((format, cpus)) = built_for(header) else { return Some("not a shared library".into()) };
    let own = own_format();
    if format != own {
        return Some(format!("built for {}, not {}", format.os(), own.os()));
    }
    let arch = std::env::consts::ARCH;
    let cpus: Vec<&str> = cpus.into_iter().collect::<Option<_>>().filter(|_| cpu(|c| c.name == arch).is_some())?;
    (!cpus.contains(&arch)).then(|| format!("built for {}, but BarSQL runs on {arch}", cpus.join(" and ")))
}

// Only a hint after a failed load: Mach-O can store exported names in pieces.
fn missing_entry_point(file: &Path, bytes: &[u8]) -> Option<String> {
    let names = ["sqlite3_extension_init".to_string(), entry_point(file, false), entry_point(file, true)];
    let found = names.iter().any(|name| bytes.windows(name.len()).any(|window| window == name.as_bytes()));
    (!found).then(|| format!("no {} entry point; give the file back its original name", names[1]))
}

// SQLite's own rule: sqlite3_, the file name's letters (or letters and digits) before the first dot, minus "lib".
fn entry_point(file: &Path, digits: bool) -> String {
    let name = file.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let name = match name.get(..3) {
        Some(lib) if lib.eq_ignore_ascii_case("lib") => &name[3..],
        _ => &name,
    };
    let stem: String = name
        .chars()
        .take_while(|c| *c != '.')
        .filter(|c| c.is_ascii_alphabetic() || (digits && c.is_ascii_digit()))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    format!("sqlite3_{stem}_init")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Elf,
    MachO,
    Pe,
}

impl Format {
    fn os(self) -> &'static str {
        match self {
            Self::Elf => "Linux",
            Self::MachO => "macOS",
            Self::Pe => "Windows",
        }
    }
}

fn own_format() -> Format {
    if cfg!(target_os = "macos") {
        Format::MachO
    } else if cfg!(windows) {
        Format::Pe
    } else {
        Format::Elf
    }
}

// Named as std::env::consts::ARCH names them.
struct Cpu {
    name: &'static str,
    elf: u16,
    pe: u16,
    mach: u32,
}

const CPUS: [Cpu; 4] = [
    Cpu { name: "x86_64", elf: 62, pe: 0x8664, mach: 0x0100_0007 },
    Cpu { name: "aarch64", elf: 183, pe: 0xaa64, mach: 0x0100_000c },
    Cpu { name: "x86", elf: 3, pe: 0x14c, mach: 7 },
    Cpu { name: "arm", elf: 40, pe: 0x1c4, mach: 12 },
];

fn cpu(matches: impl Fn(&Cpu) -> bool) -> Option<&'static str> {
    CPUS.iter().find(|cpu| matches(cpu)).map(|cpu| cpu.name)
}

fn built_for(bytes: &[u8]) -> Option<(Format, Vec<Option<&'static str>>)> {
    let u16_at = |at: usize, big: bool| {
        let raw = bytes.get(at..at + 2)?.try_into().ok()?;
        Some(if big { u16::from_be_bytes(raw) } else { u16::from_le_bytes(raw) })
    };
    let u32_at = |at: usize, big: bool| {
        let raw = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if big { u32::from_be_bytes(raw) } else { u32::from_le_bytes(raw) })
    };
    match bytes.get(..4)? {
        [0x7f, b'E', b'L', b'F'] => {
            let machine = u16_at(18, bytes.get(5) == Some(&2))?;
            Some((Format::Elf, vec![cpu(|c| c.elf == machine)]))
        }
        // The PE header can sit past what was read, which leaves the CPU unknown.
        [b'M', b'Z', ..] => {
            let machine = u32_at(0x3c, false).map(|pe| pe as usize).and_then(|pe| {
                (bytes.get(pe..pe + 4)? == b"PE\0\0").then_some(())?;
                u16_at(pe + 4, false)
            });
            Some((Format::Pe, vec![machine.and_then(|machine| cpu(|c| c.pe == machine))]))
        }
        [0xcf | 0xce, 0xfa, 0xed, 0xfe] => {
            let machine = u32_at(4, false)?;
            Some((Format::MachO, vec![cpu(|c| c.mach == machine)]))
        }
        [0xca, 0xfe, 0xba, wide @ (0xbe | 0xbf)] => {
            let count = u32_at(4, true)? as usize;
            // Java class files share the magic, with a version where the count would be.
            if count == 0 || count > 16 {
                return None;
            }
            let entry = if *wide == 0xbf { 32 } else { 20 };
            let machines = (0..count).map(|ix| u32_at(8 + ix * entry, true)).collect::<Option<Vec<_>>>()?;
            Some((Format::MachO, machines.into_iter().map(|machine| cpu(|c| c.mach == machine)).collect()))
        }
        _ => None,
    }
}

// On Unix the library already opened, so this is the entry point SQLite takes from the file name, or its init.
fn load_failure(file: &Path, err: &rusqlite::Error) -> String {
    let message = match err {
        rusqlite::Error::SqliteFailure(_, Some(message)) => message.clone(),
        other => other.to_string(),
    };
    if message.starts_with("error during initialization") {
        return message;
    }
    std::fs::read(file).ok().and_then(|bytes| missing_entry_point(file, &bytes)).unwrap_or(message)
}

// Opened before SQLite loads it. A failure keeps the loader's own reason (SQLite retries with the suffix added again
// and reports only that), and SQLite's load reuses the library instead of running its initializers again.
#[cfg(unix)]
struct Library(*mut std::ffi::c_void);

#[cfg(unix)]
impl Library {
    fn open(file: &Path) -> Result<Self, String> {
        use std::ffi::{CStr, CString};
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(file.as_os_str().as_bytes()).map_err(|err| err.to_string())?;
        // SAFETY: runs the library's initializers, as SQLite's load would, with SQLite's flags.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if !handle.is_null() {
            return Ok(Self(handle));
        }
        // SAFETY: dlerror's text stays valid until the next dl call on this thread.
        let error = unsafe { libc::dlerror() };
        match error.is_null() {
            true => Err("the library didn't load".into()),
            false => Err(clean_dlerror(&unsafe { CStr::from_ptr(error) }.to_string_lossy())),
        }
    }
}

#[cfg(unix)]
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: a handle dlopen returned, closed once.
        unsafe { libc::dlclose(self.0) };
    }
}

// macOS lists every path it tried. The first, the file itself, carries the real reason.
#[cfg(unix)]
fn clean_dlerror(error: &str) -> String {
    let line = error.lines().next().unwrap_or(error);
    let line = match line.strip_prefix("dlopen(") {
        Some(rest) => rest.split_once("): ").map_or(line, |(_, rest)| rest),
        None => line,
    };
    let Some((_, rest)) = line.strip_prefix("tried: '").and_then(|rest| rest.split_once("' (")) else {
        return line.to_string();
    };
    match rest.split_once("), '") {
        Some((reason, _)) => reason.to_string(),
        None => rest.strip_suffix(')').unwrap_or(rest).to_string(),
    }
}

// Gatekeeper's refusal of a downloaded library reads as a code signature problem.
#[cfg(target_os = "macos")]
fn with_quarantine_hint(file: &Path, reason: String) -> String {
    match reason.contains("code signature") && quarantined(file) {
        true => format!("{reason}. macOS blocks it as a download until xattr -d com.apple.quarantine clears it"),
        false => reason,
    }
}

#[cfg(target_os = "macos")]
fn quarantined(file: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = CString::new(file.as_os_str().as_bytes()) else { return false };
    // SAFETY: a size query, with no buffer to fill.
    unsafe { libc::getxattr(name.as_ptr(), c"com.apple.quarantine".as_ptr(), std::ptr::null_mut(), 0, 0, 0) >= 0 }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn with_quarantine_hint(_: &Path, reason: String) -> String {
    reason
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Format, built_for, entry_point, load, mismatched, missing_entry_point, own_format, resolve};

    fn elf(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0x7f, b'E', b'L', b'F', 2, 1];
        bytes.resize(18, 0);
        bytes.extend(machine.to_le_bytes());
        bytes
    }

    fn mach(cpu: u32) -> Vec<u8> {
        [[0xcf, 0xfa, 0xed, 0xfe], cpu.to_le_bytes()].concat()
    }

    fn fat(cpus: &[u32]) -> Vec<u8> {
        let mut bytes = [[0xca, 0xfe, 0xba, 0xbe], (cpus.len() as u32).to_be_bytes()].concat();
        for cpu in cpus {
            bytes.extend(cpu.to_be_bytes());
            bytes.extend([0; 16]);
        }
        bytes
    }

    fn pe(machine: u16) -> Vec<u8> {
        let mut bytes = vec![b'M', b'Z'];
        bytes.resize(0x3c, 0);
        bytes.extend(0x80u32.to_le_bytes());
        bytes.resize(0x80, 0);
        bytes.extend(b"PE\0\0");
        bytes.extend(machine.to_le_bytes());
        bytes
    }

    fn reason<T: std::fmt::Debug>(result: Result<T, barsql_core::QueryError>) -> String {
        result.unwrap_err().extension.expect("an extension failure").reason
    }

    #[test]
    fn headers_name_their_format_and_cpus() {
        assert_eq!(built_for(&elf(62)), Some((Format::Elf, vec![Some("x86_64")])));
        assert_eq!(built_for(&elf(183)), Some((Format::Elf, vec![Some("aarch64")])));
        assert_eq!(built_for(&elf(243)), Some((Format::Elf, vec![None])), "RISC-V");
        assert_eq!(built_for(&mach(0x0100_000c)), Some((Format::MachO, vec![Some("aarch64")])));
        let universal = Some((Format::MachO, vec![Some("x86_64"), Some("aarch64")]));
        assert_eq!(built_for(&fat(&[0x0100_0007, 0x0100_000c])), universal);
        assert_eq!(built_for(&pe(0x8664)), Some((Format::Pe, vec![Some("x86_64")])));
        assert_eq!(built_for(&pe(0xaa64)), Some((Format::Pe, vec![Some("aarch64")])));
        assert_eq!(built_for(b"-- not a library"), None);
        assert_eq!(built_for(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52]), None, "a Java class file");
        assert_eq!(built_for(&pe(0x8664)[..0x80]), Some((Format::Pe, vec![None])), "PE header out of reach");
    }

    #[test]
    fn only_a_library_for_this_os_and_cpu_passes() {
        let own = match std::env::consts::ARCH {
            "aarch64" => (elf(183), mach(0x0100_000c), pe(0xaa64)),
            _ => (elf(62), mach(0x0100_0007), pe(0x8664)),
        };
        let native = match own_format() {
            Format::Elf => own.0,
            Format::MachO => own.1,
            Format::Pe => own.2,
        };
        assert_eq!(mismatched(&native), None);
        let other_cpu = match (own_format(), std::env::consts::ARCH) {
            (Format::Elf, "aarch64") => elf(62),
            (Format::Elf, _) => elf(183),
            (Format::MachO, "aarch64") => mach(0x0100_0007),
            (Format::MachO, _) => mach(0x0100_000c),
            (Format::Pe, "aarch64") => pe(0x8664),
            (Format::Pe, _) => pe(0xaa64),
        };
        let reason = mismatched(&other_cpu).unwrap();
        assert!(reason.ends_with(&format!(", but BarSQL runs on {}", std::env::consts::ARCH)), "{reason}");
        let other_os = if own_format() == Format::Pe { elf(62) } else { pe(0x8664) };
        assert!(mismatched(&other_os).unwrap().starts_with("built for "));
        assert_eq!(mismatched(b"plain text").as_deref(), Some("not a shared library"));
        let unknown_cpu = match own_format() {
            Format::Elf => elf(243),
            Format::MachO => mach(0x0100_0012),
            Format::Pe => pe(0x5064),
        };
        assert_eq!(mismatched(&unknown_cpu), None, "left for the loader");
    }

    #[test]
    fn entry_points_come_from_the_file_name_as_sqlite_makes_them() {
        assert_eq!(entry_point(Path::new("/x/crsqlite.dylib"), false), "sqlite3_crsqlite_init");
        assert_eq!(entry_point(Path::new("/usr/lib/libExample5.4.3.so"), false), "sqlite3_example_init");
        assert_eq!(entry_point(Path::new("/usr/lib/libExample5.4.3.so"), true), "sqlite3_example5_init");
        assert_eq!(entry_point(Path::new("mod_spatialite.dylib"), false), "sqlite3_modspatialite_init");
        let renamed = Path::new("/x/crsqlite-darwin-aarch64.dylib");
        assert_eq!(missing_entry_point(Path::new("/x/crsqlite.dylib"), b"..._sqlite3_crsqlite_init..."), None);
        assert_eq!(
            missing_entry_point(renamed, b"..._sqlite3_crsqlite_init...").as_deref(),
            Some("no sqlite3_crsqlitedarwinaarch_init entry point; give the file back its original name")
        );
        assert_eq!(missing_entry_point(renamed, b"sqlite3_extension_init"), None, "the default name");
    }

    #[test]
    fn a_path_without_the_suffix_finds_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(format!("ext.{}", std::env::consts::DLL_EXTENSION));
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(resolve(&file), Some(file.clone()));
        assert_eq!(resolve(&dir.path().join("ext")), Some(file));
        assert_eq!(resolve(&dir.path().join("other")), None);
    }

    #[test]
    fn failures_name_the_file_and_why() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let missing = dir.path().join("missing.dylib");
        let err = load(&conn, std::slice::from_ref(&missing)).unwrap_err();
        assert_eq!(err.message, "can't load extension missing.dylib: file not found");
        let failure = err.extension.unwrap();
        assert_eq!((failure.path, failure.reason.as_str()), (missing.display().to_string(), "file not found"));
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "hello").unwrap();
        assert_eq!(reason(load(&conn, &[text])), "not a shared library");
    }

    #[cfg(unix)]
    #[test]
    fn os_errors_keep_the_reason_for_the_file_itself() {
        use super::clean_dlerror;
        let mac = "dlopen(/x/c.dylib, 0x0006): tried: '/x/c.dylib' (code signature in <1> '/x/c.dylib' not valid for \
                   use in process: library load disallowed by system policy), '/System/Volumes/Preboot/Cryptexes/OS/x/\
                   c.dylib' (no such file)";
        assert_eq!(
            clean_dlerror(mac),
            "code signature in <1> '/x/c.dylib' not valid for use in process: library load disallowed by system policy"
        );
        let deps = "dlopen(/x/m.dylib, 0x0006): Library not loaded: /opt/lib/libgeos_c.1.dylib\n  Referenced from: x";
        assert_eq!(clean_dlerror(deps), "Library not loaded: /opt/lib/libgeos_c.1.dylib");
        let linux = "/lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.38' not found (required by /x/c.so)";
        assert_eq!(clean_dlerror(linux), linux);
    }

    // Built with the C compiler the bundled SQLite already needs. Without SQLite's header, create_function is the
    // 46th routine in its table, which only grows at the end.
    #[cfg(unix)]
    fn build(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        build_with(dir, name, "", body)
    }

    #[cfg(unix)]
    fn build_with(dir: &Path, name: &str, prelude: &str, body: &str) -> std::path::PathBuf {
        let source = dir.join(format!("{name}.c"));
        let entry = super::entry_point(&dir.join(name), false);
        let api = "typedef struct { void *before[45]; int (*create_function)(void *, const char *, int, int, void *, \
                   void (*)(void *, int, void **), void (*)(void *, int, void **), void (*)(void *)); } api_t;\n\
                   void barsql_noop(void *ctx, int argc, void **argv) {}\n";
        let code = format!("{api}{prelude}int {entry}(void *db, char **err, const api_t *api) {{ {body} }}\n");
        std::fs::write(&source, code).unwrap();
        let library = dir.join(format!("{name}.{}", std::env::consts::DLL_EXTENSION));
        let shared = if cfg!(target_os = "macos") { "-dynamiclib" } else { "-shared" };
        let status = std::process::Command::new("cc")
            .args([shared, "-fPIC", "-o"])
            .arg(&library)
            .arg(&source)
            .status()
            .expect("run cc");
        assert!(status.success(), "cc failed for {name}");
        library
    }

    #[cfg(unix)]
    #[test]
    fn real_libraries_load_or_say_why_not() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let ok = build(dir.path(), "barsqlok", "return 0;");
        assert_eq!(load(&conn, std::slice::from_ref(&ok)).expect("loads"), [], "it adds no functions");

        let renamed = dir.path().join(format!("barsqlok-renamed.{}", std::env::consts::DLL_EXTENSION));
        std::fs::copy(&ok, &renamed).unwrap();
        let renamed = reason(load(&conn, &[renamed]));
        assert_eq!(renamed, "no sqlite3_barsqlokrenamed_init entry point; give the file back its original name");

        let failing = build(dir.path(), "barsqlfail", "return 1;");
        let failed = reason(load(&conn, &[failing]));
        assert!(failed.starts_with("error during initialization"), "{failed}");

        let sql = conn.query_row("SELECT load_extension(?1)", [ok.display().to_string()], |_| Ok(()));
        assert!(sql.is_err(), "SQL still can't load extensions afterwards");

        let cut = dir.path().join(format!("barsqlcut.{}", std::env::consts::DLL_EXTENSION));
        let bytes = std::fs::read(&ok).unwrap();
        std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
        let reason = reason(load(&conn, &[cut]));
        assert!(!reason.contains(&format!(".{0}.{0}", std::env::consts::DLL_EXTENSION)), "the loader's own: {reason}");
        assert_ne!(reason, "not a shared library", "past the header check");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_load_runs_the_librarys_initializers_once() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let count = format!(
            "#include <stdio.h>\n__attribute__((constructor)) static void count(void) {{ FILE *f = fopen(\"{}\", \"a\"); \
             if (f) {{ fputc('x', f); fclose(f); }} }}\n",
            runs.display()
        );
        let library = build_with(dir.path(), "barsqlonce", &count, "return 0;");
        let renamed = dir.path().join(format!("barsqlonce-renamed.{}", std::env::consts::DLL_EXTENSION));
        std::fs::copy(&library, &renamed).unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        assert!(reason(load(&conn, &[renamed])).starts_with("no "), "SQLite finds no entry point");
        assert_eq!(std::fs::read_to_string(&runs).unwrap(), "x");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_a_blocked_download_gets_the_quarantine_hint() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        use super::with_quarantine_hint;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("downloaded.dylib");
        std::fs::write(&file, b"x").unwrap();
        let blocked = "code signature in <1> not valid for use in process: library load disallowed by system policy";
        assert_eq!(with_quarantine_hint(&file, blocked.into()), blocked, "not downloaded");
        let name = CString::new(file.as_os_str().as_bytes()).unwrap();
        let value = b"0081;00000000;BarSQL;";
        // SAFETY: a valid path, attribute name and value.
        let set = unsafe {
            libc::setxattr(name.as_ptr(), c"com.apple.quarantine".as_ptr(), value.as_ptr().cast(), value.len(), 0, 0)
        };
        assert_eq!(set, 0);
        assert!(with_quarantine_hint(&file, blocked.into()).ends_with("xattr -d com.apple.quarantine clears it"));
        let missing = "Library not loaded: /opt/lib/libgeos_c.1.dylib";
        assert_eq!(with_quarantine_hint(&file, missing.into()), missing, "another cause");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn extension_functions_say_which_extension_they_come_from() {
        use super::super::{SqliteConnectOptions, SqliteEngine};
        let dir = tempfile::tempdir().unwrap();
        let register = r#"return api->create_function(db, "barsql_hello", 0, 1, 0, barsql_noop, 0, 0);"#;
        let library = build(dir.path(), "barsqlfn", register);
        let options =
            SqliteConnectOptions { path: dir.path().join("fn.db"), read_only: false, extensions: vec![library] };
        let engine = SqliteEngine::connect(options).await.expect("connects");
        let functions = engine.list_functions().await.unwrap().functions;
        let find = |name: &str| functions.iter().find(|f| f.name == name).map(|f| (f.builtin, f.source.as_str()));
        assert_eq!(find("barsql_hello"), Some((false, "barsqlfn")));
        assert_eq!(find("abs"), Some((true, "")));
    }
}
