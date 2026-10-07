mod docs_images;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

const USAGE: &str = "\
usage: cargo xtask lint
       cargo xtask e2e [up | down | logs | all]
       cargo xtask screenshots
       cargo xtask docs-images
       cargo xtask package
       cargo xtask version
       cargo xtask bump-version [--dry-run] (--major | --minor | --patch | <version>)

lint       cargo fmt --check, then clippy with the e2e and snapshot features and -D warnings, as CI runs them.
e2e        The PostgreSQL, MySQL, MariaDB, libSQL and ClickHouse suites, against the stack in docker-compose.yml. up,
           down and logs manage the stack (COMPOSE=\"podman compose\" swaps the tool); all brings it up, runs the
           suites and tears it down. BARSQL_E2E_MSSQL=1 adds SQL Server, as Azure SQL Edge on arm64.
screenshots
           The README screenshots, in .github/screenshots. Restores fixtures/screenshots/forum.sql into the
           stack's PostgreSQL, then plays the scenes in a snapshot build on a copy of
           fixtures/screenshots/data, and makes the landing page's images from them (docs-images).
docs-images
           The landing page's images in docs/: each README screenshot as a full-size lossless WebP plus 640, 800
           and 1280px copies, a 1200 × 630 link preview, and the app icon.
package    This OS's release packages, in target/package/dist.

The version lives in [workspace.package] in Cargo.toml; Cargo.lock repeats it for every workspace
crate, and the package scripts read it from there.

  --major     1.5.3 -> 2.0.0
  --minor     1.5.3 -> 1.6.0
  --patch     1.5.3 -> 1.5.4
  <version>   an explicit version, e.g. 1.6.0";

const LINT_FEATURES: &str = "barsql-db/e2e,barsql-app/e2e,barsql-ui/e2e,barsql/snapshot";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("lint") => lint(),
        Some("e2e") => e2e(args.get(1).map(String::as_str)),
        Some("screenshots") => screenshots(),
        Some("docs-images") => docs_images::run(&workspace()),
        Some("package") => package(),
        Some("bump-version") => bump_version(&args[1..]),
        Some("version") => current(&workspace()).map(|version| println!("{version}")),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    run_with(program, args, None, &[])
}

// `stdin` feeds the program a file.
fn run_with(program: &str, args: &[&str], stdin: Option<&Path>, envs: &[(&str, &Path)]) -> Result<(), String> {
    let name = Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or(program);
    eprintln!("> {name} {}", args.join(" "));
    let mut command = Command::new(program);
    command.args(args).current_dir(workspace()).envs(envs.iter().copied());
    if let Some(path) = stdin {
        command.stdin(fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?);
    }
    let status = command.status().map_err(|e| format!("{program}: {e}"))?;
    status.success().then_some(()).ok_or_else(|| format!("{program} {}: {status}", args.join(" ")))
}

fn cargo(args: &[&str]) -> Result<(), String> {
    run(&env::var("CARGO").unwrap_or_else(|_| "cargo".into()), args)
}

fn lint() -> Result<(), String> {
    cargo(&["fmt", "--all", "--check"])?;
    cargo(&["clippy", "--workspace", "--all-targets", "--locked", "--features", LINT_FEATURES, "--", "-D", "warnings"])
}

fn e2e(step: Option<&str>) -> Result<(), String> {
    match step {
        None => e2e_suites(),
        Some("up") => compose(&["up", "-d", "--wait"]),
        Some("down") => compose(&["down", "-v"]),
        Some("logs") => compose(&["logs", "--no-color"]),
        Some("all") => {
            let suites = compose(&["up", "-d", "--wait"]).and_then(|()| e2e_suites());
            let down = compose(&["down", "-v"]);
            suites.and(down)
        }
        Some(_) => Err(USAGE.to_string()),
    }
}

fn e2e_suites() -> Result<(), String> {
    cargo(&["test", "-p", "barsql-db", "-p", "barsql-app", "--features", "barsql-db/e2e,barsql-app/e2e"])?;
    cargo(&["test", "-p", "barsql-ui", "--features", "e2e", "scenarios::engines"])
}

fn compose(args: &[&str]) -> Result<(), String> {
    compose_with(args, None)
}

fn compose_with(args: &[&str], stdin: Option<&Path>) -> Result<(), String> {
    let tool = env::var("COMPOSE").unwrap_or_else(|_| "docker compose".into());
    let mut words = tool.split_whitespace();
    let program = words.next().ok_or("COMPOSE is empty")?;
    let args: Vec<&str> = words.chain(args.iter().copied()).collect();
    run_with(program, &args, stdin, &mssql_env())
}

// SQL Server's image is amd64 only. Azure SQL Edge runs the same engine on arm64, so it stands in there.
fn mssql_env() -> Vec<(&'static str, &'static Path)> {
    if env::var("BARSQL_E2E_MSSQL").as_deref() != Ok("1") {
        return Vec::new();
    }
    let mut envs = vec![("COMPOSE_PROFILES", Path::new("mssql"))];
    if env::consts::ARCH == "aarch64" && env::var_os("MSSQL_IMAGE").is_none() {
        envs.push(("MSSQL_IMAGE", Path::new("mcr.microsoft.com/azure-sql-edge:latest")));
        envs.push(("MSSQL_PLATFORM", Path::new("linux/arm64")));
    }
    envs
}

// Recreate the forum database every run. The scenes insert and roll back, which still moves its sequences.
fn screenshots() -> Result<(), String> {
    let root = workspace();
    let fixtures = root.join("fixtures/screenshots");
    compose(&["up", "-d", "--wait", "postgres"])?;
    let psql = ["exec", "-T", "postgres", "psql", "-U", "postgres", "-q", "-v", "ON_ERROR_STOP=1"];
    compose(&[&psql[..], &["-c", "DROP DATABASE IF EXISTS forum WITH (FORCE)"]].concat())?;
    compose(&[&psql[..], &["-c", "CREATE DATABASE forum"]].concat())?;
    compose_with(&[&psql[..], &["-d", "forum"]].concat(), Some(&fixtures.join("forum.sql")))?;
    let data = root.join("target/screenshots/BarSQL-data");
    if data.exists() {
        fs::remove_dir_all(&data).map_err(|e| format!("{}: {e}", data.display()))?;
    }
    copy_dir(&fixtures.join("data"), &data)?;
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let envs = [
        ("BARSQL_DATA_DIR", data.as_path()),
        ("BARSQL_SCREENSHOTS", Path::new(".github/screenshots")),
        ("BARSQL_SNAPSHOT_SIZE", Path::new("2048x1152")),
    ];
    run_with(&cargo, &["run", "-p", "barsql", "--features", "snapshot"], None, &envs)?;
    docs_images::run(&root)
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let path = entry.map_err(|e| format!("{}: {e}", from.display()))?.path();
        let target = to.join(path.file_name().unwrap_or_default());
        if path.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            fs::copy(&path, &target).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

fn package() -> Result<(), String> {
    let script = format!("scripts/package-{}.sh", env::consts::OS);
    if !workspace().join(&script).exists() {
        return Err(format!("no {script} for this OS"));
    }
    run("bash", &[&script])
}

fn current(root: &Path) -> Result<String, String> {
    let manifest = read(&root.join("Cargo.toml"))?;
    workspace_version(&manifest).map(str::to_string).ok_or("no version in [workspace.package]".into())
}

fn bump_version(args: &[String]) -> Result<(), String> {
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let rest: Vec<&str> = args.iter().map(String::as_str).filter(|arg| *arg != "--dry-run").collect();
    let [request] = rest.as_slice() else { return Err(USAGE.to_string()) };
    let root = workspace();
    let (manifest_path, lock_path) = (root.join("Cargo.toml"), root.join("Cargo.lock"));
    let manifest = read(&manifest_path)?;
    let old = workspace_version(&manifest).ok_or("no version in [workspace.package]")?.to_string();
    let new = next_version(&old, request)?;
    if new == old {
        return Err(format!("the version is already {old}"));
    }
    let manifest = set_workspace_version(&manifest, &new)?;
    let lock = set_lock_versions(&read(&lock_path)?, &old, &new);
    println!("{old} -> {new}");
    if dry_run {
        println!("dry run: nothing written");
        return Ok(());
    }
    write(&manifest_path, &manifest)?;
    write(&lock_path, &lock)?;
    println!("updated Cargo.toml and Cargo.lock");
    println!("next: commit, then git tag v{new} && git push origin v{new}");
    Ok(())
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[derive(Debug, PartialEq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

fn parse(text: &str) -> Option<Version> {
    let mut parts = text.split('.').map(|part| {
        let simple =
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) && (part == "0" || !part.starts_with('0'));
        simple.then(|| part.parse::<u64>().ok()).flatten()
    });
    let (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    Some(Version { major, minor, patch })
}

fn next_version(current: &str, request: &str) -> Result<String, String> {
    let Some(Version { major, minor, patch }) = parse(current) else {
        return Err(format!("the current version {current:?} is not X.Y.Z"));
    };
    let next = match request {
        "--major" => format!("{}.0.0", major + 1),
        "--minor" => format!("{major}.{}.0", minor + 1),
        "--patch" => format!("{major}.{minor}.{}", patch + 1),
        flag if flag.starts_with("--") => return Err(USAGE.to_string()),
        explicit => {
            let explicit = explicit.strip_prefix('v').unwrap_or(explicit);
            parse(explicit).ok_or_else(|| format!("{explicit:?} is not X.Y.Z"))?;
            explicit.to_string()
        }
    };
    Ok(next)
}

fn workspace_package(manifest: &str) -> impl Iterator<Item = &str> {
    manifest
        .lines()
        .skip_while(|line| line.trim() != "[workspace.package]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
}

fn version_value(line: &str) -> Option<&str> {
    let value = line.strip_prefix("version")?.trim_start().strip_prefix('=')?.trim();
    value.strip_prefix('"')?.strip_suffix('"')
}

fn workspace_version(manifest: &str) -> Option<&str> {
    workspace_package(manifest).find_map(version_value)
}

fn set_workspace_version(manifest: &str, new: &str) -> Result<String, String> {
    let line = workspace_package(manifest)
        .find(|line| version_value(line).is_some())
        .ok_or("no version in [workspace.package]")?;
    let at = line.as_ptr() as usize - manifest.as_ptr() as usize;
    Ok(format!("{}version = \"{new}\"{}", &manifest[..at], &manifest[at + line.len()..]))
}

// Lock entries without a `source` are the workspace's own crates.
fn set_lock_versions(lock: &str, old: &str, new: &str) -> String {
    let (from, to) = (format!("version = \"{old}\""), format!("version = \"{new}\""));
    let mut out = String::with_capacity(lock.len());
    for (i, block) in lock.split("\n\n").enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        let local = block.starts_with("[[package]]") && !block.lines().any(|line| line.starts_with("source = "));
        for line in block.split_inclusive('\n') {
            match line.strip_suffix('\n').unwrap_or(line) {
                text if local && text == from => out.push_str(&line.replacen(&from, &to, 1)),
                _ => out.push_str(line),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_bump_raises_one_part() {
        let cases = [
            ("1.5.3", "--major", "2.0.0"),
            ("1.5.3", "--minor", "1.6.0"),
            ("1.5.3", "--patch", "1.5.4"),
            ("0.1.0", "2.0.0", "2.0.0"),
            ("0.1.0", "v2.0.0", "2.0.0"),
        ];
        for (current, request, next) in cases {
            assert_eq!(next_version(current, request).as_deref(), Ok(next), "{current} {request}");
        }
    }

    #[test]
    fn bad_versions_and_requests_are_refused() {
        for request in ["2.0", "2.0.0.0", "02.0.0", "2.0.0-", "2.0.0-rc.1", "2.0.0+build", "--huge"] {
            assert!(next_version("1.5.3", request).is_err(), "{request}");
        }
        assert!(next_version("2.0.0-rc.1", "--patch").is_err(), "the current version must be X.Y.Z");
    }

    #[test]
    fn only_the_workspace_version_and_its_lock_entries_change() {
        let manifest = "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\nfoo = { version = \"0.1.0\" }\n";
        assert_eq!(workspace_version(manifest), Some("0.1.0"));
        let bumped = set_workspace_version(manifest, "2.0.0").unwrap();
        assert_eq!(bumped, manifest.replacen("version = \"0.1.0\"\nedition", "version = \"2.0.0\"\nedition", 1));

        let lock = "version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"0.1.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"barsql\"\nversion = \"0.1.0\"\ndependencies = [\n \"serde\",\n]\n";
        let expected =
            lock.replacen("name = \"barsql\"\nversion = \"0.1.0\"", "name = \"barsql\"\nversion = \"2.0.0\"", 1);
        assert_eq!(set_lock_versions(lock, "0.1.0", "2.0.0"), expected);
    }
}
