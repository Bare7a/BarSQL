use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

pub const GITHUB_API: &str = "https://api.github.com";
pub const REPOSITORY: &str = "Bare7a/BarSQL";
const CHECKSUM_ASSET: &str = "SHA256SUMS";
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    // Without the tag's `v`.
    pub version: String,
    pub name: String,
    pub notes: String,
    pub html_url: String,
    pub asset: Asset,
    pub digest: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Asset {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default, rename = "browser_download_url")]
    pub url: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

// How this copy was installed. It decides which asset replaces it and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    // The platform .zip or .tar.gz, swapped in for the .app or the binary.
    Archive,
    AppImage,
    Deb,
    Rpm,
    Pacman,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Archive => "asset",
            Self::AppImage => "AppImage",
            Self::Deb => ".deb",
            Self::Rpm => ".rpm",
            Self::Pacman => "Arch package",
        }
    }

    pub(crate) fn package(self) -> Option<&'static str> {
        match self {
            Self::Deb => Some("deb"),
            Self::Rpm => Some("rpm"),
            Self::Pacman => Some("pacman"),
            Self::Archive | Self::AppImage => None,
        }
    }

    pub(crate) fn from_package(name: &str) -> Option<Self> {
        [Self::Deb, Self::Rpm, Self::Pacman].into_iter().find(|kind| kind.package() == Some(name))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    pub kind: Kind,
    // What the update replaces, or relaunches after a package install.
    pub target: PathBuf,
}

// Detected once, because neither the executable nor its package changes while the app runs.
pub fn install() -> &'static Install {
    static INSTALL: OnceLock<Install> = OnceLock::new();
    INSTALL.get_or_init(|| {
        let exe = std::env::current_exe().map(|exe| std::fs::canonicalize(&exe).unwrap_or(exe)).unwrap_or_default();
        detect(std::env::consts::OS, &exe, appimage_file(&exe), owning_package)
    })
}

pub(crate) fn detect(
    os: &str,
    exe: &Path,
    appimage: Option<PathBuf>,
    owner: impl Fn(&Path) -> Option<Kind>,
) -> Install {
    if os == "linux" {
        if let Some(target) = appimage {
            return Install { kind: Kind::AppImage, target };
        }
        if exe.starts_with("/usr")
            && let Some(kind) = owner(exe)
        {
            return Install { kind, target: exe.to_path_buf() };
        }
    }
    Install { kind: Kind::Archive, target: bundle_target(exe) }
}

pub(crate) fn appimage_file(exe: &Path) -> Option<PathBuf> {
    appimage(exe, std::env::var_os("APPIMAGE"), std::env::var_os("APPDIR"))
}

// Apps started from an AppImage inherit APPIMAGE, so the executable must also sit in its mount.
// has_root (not is_absolute) so Unix APPIMAGE paths still count when these unit tests run on Windows.
pub(crate) fn appimage(exe: &Path, file: Option<OsString>, mount: Option<OsString>) -> Option<PathBuf> {
    let (file, mount) = (PathBuf::from(file?), mount?);
    (file.has_root() && !mount.is_empty() && exe.starts_with(mount)).then_some(file)
}

fn owning_package(exe: &Path) -> Option<Kind> {
    let queries = [
        (Kind::Deb, "/usr/bin/dpkg-query", "-S"),
        (Kind::Rpm, "/usr/bin/rpm", "-qf"),
        (Kind::Pacman, "/usr/bin/pacman", "-Qo"),
    ];
    queries.into_iter().find_map(|(kind, tool, flag)| {
        let owned = Path::new(tool).exists()
            && Command::new(tool)
                .arg(flag)
                .arg(exe)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
        owned.then_some(kind)
    })
}

// Named the way the release assets spell them, not Rust's target names.
pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

pub fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        other => other,
    }
}

pub fn check(base: &str, current: &str, kind: Kind, platform: &str, arch: &str) -> Result<Option<Release>, String> {
    let agent = agent();
    let endpoint = format!("{base}/repos/{REPOSITORY}/releases/latest");
    let Some(release) = fetch_release(&agent, &endpoint)? else { return Ok(None) };
    if !is_newer(&release.tag_name, current) {
        return Ok(None);
    }
    let Some(ix) = pick_asset(kind, platform, arch, &release.assets) else {
        return Err(format!("github: release {} has no {} for {platform}/{arch}", release.tag_name, kind.label()));
    };
    let asset = release.assets[ix].clone();
    let digest = match release.assets.iter().find(|a| a.name == CHECKSUM_ASSET) {
        Some(sums) => {
            fetch_checksum(&agent, &sums.url, &asset.name).map_err(|e| format!("github: load checksum sidecar: {e}"))?
        }
        None => None,
    };
    Ok(Some(Release {
        version: trim_prefix(&release.tag_name).to_string(),
        name: release.name.unwrap_or_default(),
        notes: release.body.unwrap_or_default(),
        html_url: release.html_url,
        asset,
        digest,
    }))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent(format!("BarSQL/{}", crate::VERSION))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    agent.get(url).header("Accept", accept).header("X-GitHub-Api-Version", "2022-11-28").call()
}

// A 404 means nothing is published yet.
fn fetch_release(agent: &ureq::Agent, endpoint: &str) -> Result<Option<ApiRelease>, String> {
    let mut response =
        get(agent, endpoint, "application/vnd.github+json").map_err(|e| format!("github: api request: {e}"))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().map_err(|e| format!("github: api request: {e}"));
    match status {
        200 => {}
        404 => return Ok(None),
        _ => {
            let mut body = text.unwrap_or_default();
            body.truncate((0..=body.len().min(4096)).rev().find(|&at| body.is_char_boundary(at)).unwrap_or(0));
            return Err(format!("github: api {status}: {body}"));
        }
    }
    serde_json::from_str(&text?).map(Some).map_err(|e| format!("github: decode release: {e}"))
}

fn fetch_checksum(agent: &ureq::Agent, url: &str, target: &str) -> Result<Option<Vec<u8>>, String> {
    let mut response = get(agent, url, "application/octet-stream").map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("checksum sidecar HTTP {status}"));
    }
    let body = response.body_mut().with_config().limit(1 << 20).read_to_string().map_err(|e| e.to_string())?;
    parse_checksums(&body, target)
}

// Parses `sha256sum` output, "<hex digest>  <file>", where the file may carry a `*` or `./` prefix.
pub fn parse_checksums(body: &str, target: &str) -> Result<Option<Vec<u8>>, String> {
    for line in body.split('\n') {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let name = fields[fields.len() - 1];
        let name = name.strip_prefix('*').unwrap_or(name);
        let name = name.strip_prefix("./").unwrap_or(name);
        if name == target {
            return hex::decode(fields[0]).map(Some).map_err(|e| format!("malformed digest for {target}: {e}"));
        }
    }
    Ok(None)
}

pub fn pick_asset(kind: Kind, platform: &str, arch: &str, assets: &[Asset]) -> Option<usize> {
    let (platform, arch) = (platform.to_lowercase(), arch.to_lowercase());
    assets.iter().position(|asset| {
        let name = asset.name.to_lowercase();
        match kind {
            Kind::Archive => {
                !(name.ends_with(".sig") || name.ends_with(".asc"))
                    && !(name.contains("-installer.") || name.contains("_installer.") || name == "installer.exe")
                    && !is_checksum_name(&name)
                    && (platform.is_empty() || name.contains(&platform))
                    && (arch.is_empty() || contains_arch(&name, &arch))
            }
            Kind::AppImage => name.ends_with(".appimage") && (arch.is_empty() || contains_arch(&name, &arch)),
            Kind::Deb => name.ends_with(".deb"),
            Kind::Rpm => name.ends_with(".rpm"),
            Kind::Pacman => name.ends_with(".pkg.tar.zst"),
        }
    })
}

fn contains_arch(name: &str, arch: &str) -> bool {
    if name.contains(arch) {
        return true;
    }
    match arch {
        "amd64" => name.contains("x86_64") || name.contains("x64"),
        "arm64" => name.contains("aarch64"),
        "386" => {
            !(name.contains("x86_64") || name.contains("x64") || name.contains("amd64"))
                && (name.contains("i386") || name.contains("ia32") || name.contains("x86"))
        }
        _ => false,
    }
}

fn is_checksum_name(name: &str) -> bool {
    [".sha256", ".sha512", ".sums", ".checksum", ".checksums"].iter().any(|ext| name.ends_with(ext))
        || ["checksums", "sha256sums", "sha512sums"]
            .iter()
            .any(|token| name == *token || name.strip_prefix(token).is_some_and(|rest| rest.starts_with('.')))
}

pub fn trim_prefix(tag: &str) -> &str {
    tag.strip_prefix(['v', 'V']).unwrap_or(tag)
}

// Any tag is newer than an empty current version.
pub fn is_newer(tag: &str, current: &str) -> bool {
    if trim_prefix(tag).is_empty() {
        return false;
    }
    if trim_prefix(current).is_empty() {
        return true;
    }
    compare(tag, current) == Ordering::Greater
}

// MAJOR.MINOR.PATCH with an optional `v`. Invalid versions compare equal and sort below valid ones.
pub fn compare(a: &str, b: &str) -> Ordering {
    parse(a).cmp(&parse(b))
}

fn parse(version: &str) -> Option<[u64; 3]> {
    let mut parts = trim_prefix(version).split('.');
    let mut numbers = [0; 3];
    for number in &mut numbers {
        let part = parts.next()?;
        let plain =
            !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()) && (part == "0" || !part.starts_with('0'));
        *number = plain.then(|| part.parse().ok()).flatten()?;
    }
    parts.next().is_none().then_some(numbers)
}

mod admin;
mod helper;
mod install;

pub use helper::{bundle_target, helper_mode, spawn_helper, sweep_update_leftovers};
pub use install::{InstallError, InstallEvent, Stage, Staged, download_and_stage, extract_single};

#[cfg(test)]
mod tests;
