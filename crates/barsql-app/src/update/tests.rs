use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{
    Asset, Install, Kind, appimage, bundle_target, check, compare, detect, is_newer, parse_checksums, pick_asset,
};

type Routes = Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>;

// Stands in for api.github.com and its download host.
struct Server {
    base: String,
    routes: Routes,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let shared = routes.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let routes = shared.clone();
                std::thread::spawn(move || serve(stream, &routes));
            }
        });
        Self { base, routes }
    }

    fn route(&self, path: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.routes.lock().unwrap().insert(path.into(), (status, body.into()));
    }
}

fn serve(mut stream: TcpStream, routes: &Routes) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
    let (status, body) = routes.lock().unwrap().get(&path).cloned().unwrap_or((404, b"Not Found".to_vec()));
    let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(&body);
}

// Keep in sync with the assets release.yml publishes.
const RELEASE_ASSETS: [&str; 13] = [
    "SHA256SUMS",
    "BarSQL",
    "BarSQL-amd64-installer.exe",
    "BarSQL-darwin-amd64.zip",
    "BarSQL-darwin-arm64.zip",
    "BarSQL-linux-amd64.tar.gz",
    "BarSQL-macos-universal.dmg",
    "BarSQL-windows-amd64.zip",
    "BarSQL-x86_64.AppImage",
    "BarSQL.deb",
    "BarSQL.exe",
    "BarSQL.pkg.tar.zst",
    "BarSQL.rpm",
];

fn assets(names: &[&str]) -> Vec<Asset> {
    names.iter().map(|name| Asset { name: name.to_string(), ..Default::default() }).collect()
}

fn picked(platform: &str, arch: &str, names: &[&str]) -> Option<String> {
    pick_asset(Kind::Archive, platform, arch, &assets(names)).map(|ix| names[ix].to_string())
}

#[test]
fn versions_order_by_their_numbers() {
    let ascending = ["bad", "v0.9.9", "v1.0.0", "v1.0.1", "v1.2.0", "v1.2.3", "v1.10.0", "v2.0.0", "v10.0.0"];
    for pair in ascending.windows(2) {
        assert_eq!(compare(pair[0], pair[1]), Ordering::Less, "{} < {}", pair[0], pair[1]);
        assert_eq!(compare(pair[1], pair[0]), Ordering::Greater, "{} > {}", pair[1], pair[0]);
    }
    let equal = [
        ("1.2.3", "v1.2.3"),
        ("V1.2.3", "v1.2.3"),
        ("", "bad"),
        ("v1", "bad"),
        ("v1.2", "bad"),
        ("v1.2.3.4", "bad"),
        ("v1..3", "bad"),
        ("v01.2.3", "bad"),
        ("v1.2.3-rc.1", "bad"),
        ("v1.2.3+meta", "bad"),
    ];
    for (a, b) in equal {
        assert_eq!(compare(a, b), Ordering::Equal, "{a} == {b}");
    }
}

#[test]
fn newer_means_strictly_newer_with_an_optional_v() {
    assert!(is_newer("v1.5.4", "1.5.3"));
    assert!(!is_newer("1.5.3", "v1.5.3"));
    assert!(is_newer("v1.10.0", "1.9.9"));
    assert!(!is_newer("v1.5.9", "2.0.0"));
    assert!(!is_newer("", "1.0.0"));
    assert!(is_newer("v1.0.0", ""));
}

#[test]
fn each_platform_gets_its_updater_archive() {
    assert_eq!(picked("darwin", "arm64", &RELEASE_ASSETS).as_deref(), Some("BarSQL-darwin-arm64.zip"));
    assert_eq!(picked("darwin", "amd64", &RELEASE_ASSETS).as_deref(), Some("BarSQL-darwin-amd64.zip"));
    assert_eq!(picked("linux", "amd64", &RELEASE_ASSETS).as_deref(), Some("BarSQL-linux-amd64.tar.gz"));
    assert_eq!(picked("windows", "amd64", &RELEASE_ASSETS).as_deref(), Some("BarSQL-windows-amd64.zip"));
    assert_eq!(picked("linux", "arm64", &RELEASE_ASSETS), None);
    assert_eq!(picked("windows", "386", &RELEASE_ASSETS), None);
}

#[test]
fn appimages_and_packages_get_their_own_asset() {
    let pick = |kind, arch| pick_asset(kind, "linux", arch, &assets(&RELEASE_ASSETS)).map(|ix| RELEASE_ASSETS[ix]);
    assert_eq!(pick(Kind::AppImage, "amd64"), Some("BarSQL-x86_64.AppImage"));
    assert_eq!(pick(Kind::AppImage, "arm64"), None);
    assert_eq!(pick(Kind::Deb, "amd64"), Some("BarSQL.deb"));
    assert_eq!(pick(Kind::Rpm, "amd64"), Some("BarSQL.rpm"));
    assert_eq!(pick(Kind::Pacman, "amd64"), Some("BarSQL.pkg.tar.zst"));
}

#[test]
fn installs_are_told_apart() {
    let none = |_: &Path| None;
    let rpm = |_: &Path| Some(Kind::Rpm);
    let bundle = Path::new("/Applications/BarSQL.app/Contents/MacOS/BarSQL");
    assert_eq!(detect("macos", bundle, None, none), Install { kind: Kind::Archive, target: bundle_target(bundle) });
    let image = PathBuf::from("/home/u/Apps/BarSQL-x86_64.AppImage");
    let mounted = Path::new("/tmp/.mount_BarSQL/usr/bin/BarSQL");
    assert_eq!(detect("linux", mounted, Some(image.clone()), rpm), Install { kind: Kind::AppImage, target: image });
    let usr = Path::new("/usr/bin/BarSQL");
    assert_eq!(detect("linux", usr, None, rpm), Install { kind: Kind::Rpm, target: usr.into() });
    assert_eq!(detect("linux", usr, None, none), Install { kind: Kind::Archive, target: usr.into() });
    let home = Path::new("/home/u/bin/BarSQL");
    let unasked = |_: &Path| panic!("only binaries under /usr can belong to a package");
    assert_eq!(detect("linux", home, None, unasked), Install { kind: Kind::Archive, target: home.into() });
    assert_eq!(detect("windows", Path::new(r"C:\Program Files\BarSQL\BarSQL.exe"), None, rpm).kind, Kind::Archive);
}

#[test]
fn appimage_variables_count_only_inside_the_mount() {
    let exe = Path::new("/tmp/.mount_BarSQL1/usr/bin/BarSQL");
    let file = || Some(OsString::from("/home/u/BarSQL-x86_64.AppImage"));
    let mount = |dir: &str| Some(OsString::from(dir));
    assert_eq!(appimage(exe, file(), mount("/tmp/.mount_BarSQL1")), Some("/home/u/BarSQL-x86_64.AppImage".into()));
    let inherited = appimage(Path::new("/usr/bin/BarSQL"), file(), mount("/tmp/.mount_Term"));
    assert_eq!(inherited, None, "a program started from another AppImage");
    assert_eq!(appimage(exe, file(), mount("")), None);
    assert_eq!(appimage(exe, Some("BarSQL.AppImage".into()), mount("/tmp/.mount_BarSQL1")), None);
    assert_eq!(appimage(exe, None, None), None);
}

#[test]
fn signatures_checksums_and_installers_are_skipped() {
    let names = [
        "app-darwin-arm64.zip.sig",
        "checksums.txt",
        "app-darwin-arm64.sha256",
        "app-darwin-arm64-installer.dmg",
        "App-Darwin-AARCH64.zip",
    ];
    assert_eq!(picked("darwin", "arm64", &names).as_deref(), Some("App-Darwin-AARCH64.zip"));
    assert_eq!(picked("linux", "amd64", &["app-linux-x86_64.tar.gz"]).as_deref(), Some("app-linux-x86_64.tar.gz"));
    assert_eq!(
        picked("windows", "386", &["app-windows-x86_64.zip", "app-windows-x86.zip"]).as_deref(),
        Some("app-windows-x86.zip")
    );
    assert_eq!(
        picked("darwin", "arm64", &["myapp-sha256-darwin-arm64.zip"]).as_deref(),
        Some("myapp-sha256-darwin-arm64.zip")
    );
}

#[test]
fn checksum_listings_match_the_base_name() {
    let body = "# sha256sum *\nabcd  other.zip\n0a0B  *BarSQL-darwin-arm64.zip\n\n  ff00   ./BarSQL.exe  \n";
    assert_eq!(parse_checksums(body, "BarSQL-darwin-arm64.zip"), Ok(Some(vec![0x0a, 0x0b])));
    assert_eq!(parse_checksums(body, "BarSQL.exe"), Ok(Some(vec![0xff, 0x00])));
    assert_eq!(parse_checksums(body, "missing.zip"), Ok(None));
    let error = parse_checksums("zz  BarSQL.exe", "BarSQL.exe").unwrap_err();
    assert!(error.starts_with("malformed digest for BarSQL.exe"), "{error}");
}

fn release_json(base: &str, tag: &str) -> String {
    let assets: Vec<String> = RELEASE_ASSETS
        .iter()
        .map(|name| format!(r#"{{"name":"{name}","size":42,"browser_download_url":"{base}/download/{name}"}}"#))
        .collect();
    format!(
        r#"{{"tag_name":"{tag}","name":null,"body":"- Faster grids","html_url":"{base}/releases/{tag}","assets":[{}]}}"#,
        assets.join(",")
    )
}

#[test]
fn a_newer_release_comes_with_its_asset_and_digest() {
    let server = Server::start();
    let base = server.base.clone();
    server.route("/repos/Bare7a/BarSQL/releases/latest", 200, release_json(&base, "v1.6.0"));
    let sums = "beef  BarSQL-darwin-arm64.zip\ncafe  BarSQL-linux-amd64.tar.gz\nf00d  BarSQL-x86_64.AppImage\n";
    server.route("/download/SHA256SUMS", 200, sums);

    let release = check(&base, "1.5.3", Kind::Archive, "darwin", "arm64").unwrap().unwrap();
    assert_eq!(release.version, "1.6.0");
    assert_eq!((release.name.as_str(), release.notes.as_str()), ("", "- Faster grids"));
    assert_eq!(release.html_url, format!("{base}/releases/v1.6.0"));
    assert_eq!((release.asset.name.as_str(), release.asset.size), ("BarSQL-darwin-arm64.zip", 42));
    assert_eq!(release.asset.url, format!("{base}/download/BarSQL-darwin-arm64.zip"));
    assert_eq!(release.digest, Some(vec![0xbe, 0xef]));

    assert_eq!(check(&base, "v1.6.0", Kind::Archive, "darwin", "arm64"), Ok(None), "up to date");
    assert_eq!(
        check(&base, "1.5.3", Kind::Archive, "linux", "arm64"),
        Err("github: release v1.6.0 has no asset for linux/arm64".into())
    );
    let image = check(&base, "1.5.3", Kind::AppImage, "linux", "amd64").unwrap().unwrap();
    assert_eq!((image.asset.name.as_str(), image.digest), ("BarSQL-x86_64.AppImage", Some(vec![0xf0, 0x0d])));
    assert_eq!(
        check(&base, "1.5.3", Kind::AppImage, "linux", "arm64"),
        Err("github: release v1.6.0 has no AppImage for linux/arm64".into())
    );
}

#[test]
fn nothing_published_is_up_to_date_and_server_errors_are_reported() {
    let server = Server::start();
    assert_eq!(check(&server.base, "1.5.3", Kind::Archive, "darwin", "arm64"), Ok(None));
    server.route("/repos/Bare7a/BarSQL/releases/latest", 500, "boom");
    assert_eq!(check(&server.base, "1.5.3", Kind::Archive, "darwin", "arm64"), Err("github: api 500: boom".into()));
    server.route("/repos/Bare7a/BarSQL/releases/latest", 200, "{not json");
    let error = check(&server.base, "1.5.3", Kind::Archive, "darwin", "arm64").unwrap_err();
    assert!(error.starts_with("github: decode release:"), "{error}");
}

mod install {
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::fs;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, SystemTime};

    use sha2::{Digest, Sha256};
    use zip::write::SimpleFileOptions;

    use super::super::helper::{Host, Job, admin_swap, run_helper, sweep_staging};
    use super::super::install::{InstallEvent, Stage, download_and_stage, extract_single};
    use super::super::{Asset, Kind, Release, arch, bundle_target, check, platform};
    use super::Server;

    fn app_zip(entries: &[(&str, &[u8], u32)], links: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, body, mode) in entries {
            zip.start_file(*name, SimpleFileOptions::default().unix_permissions(*mode)).unwrap();
            zip.write_all(body).unwrap();
        }
        for (name, target) in links {
            zip.add_symlink(*name, *target, SimpleFileOptions::default()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn bundle() -> Vec<u8> {
        app_zip(
            &[
                ("BarSQL.app/Contents/MacOS/BarSQL", b"#!/bin/sh\necho new\n", 0o755),
                ("BarSQL.app/Contents/Info.plist", b"<plist/>", 0o644),
            ],
            &[("BarSQL.app/Contents/Current", "MacOS")],
        )
    }

    fn release(server: &Server, body: &[u8], digest: Option<Vec<u8>>) -> Release {
        release_of(server, "BarSQL-darwin-arm64.zip", body, digest)
    }

    fn release_of(server: &Server, name: &str, body: &[u8], digest: Option<Vec<u8>>) -> Release {
        server.route(&format!("/download/{name}"), 200, body.to_vec());
        Release {
            version: "9.9.9".into(),
            name: String::new(),
            notes: String::new(),
            html_url: String::new(),
            asset: Asset {
                name: name.into(),
                size: body.len() as u64,
                url: format!("{}/download/{name}", server.base),
            },
            digest,
        }
    }

    #[test]
    fn a_verified_download_is_unpacked_to_its_one_entry() {
        let server = Server::start();
        let body = bundle();
        let digest = Sha256::digest(&body).to_vec();
        let mut events = Vec::new();
        let staged = download_and_stage(&release(&server, &body, Some(digest)), &AtomicBool::new(false), |event| {
            events.push(event)
        })
        .unwrap();
        assert!(staged.path.ends_with("BarSQL.app") && staged.path.starts_with(&staged.dir));
        let binary = staged.path.join("Contents/MacOS/BarSQL");
        assert_eq!(fs::read_to_string(&binary).unwrap(), "#!/bin/sh\necho new\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&binary).unwrap().permissions().mode() & 0o777, 0o755);
            assert_eq!(fs::read_link(staged.path.join("Contents/Current")).unwrap(), Path::new("MacOS"));
        }
        assert!(matches!(events.last(), Some(InstallEvent::Installing)));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, InstallEvent::Downloading { written, .. } if *written == body.len() as u64))
        );
        fs::remove_dir_all(staged.dir).unwrap();
    }

    #[test]
    fn a_wrong_or_missing_digest_stops_before_anything_is_unpacked() {
        let server = Server::start();
        let body = bundle();
        let wrong = download_and_stage(&release(&server, &body, Some(vec![0; 32])), &AtomicBool::new(false), |_| {})
            .unwrap_err();
        assert_eq!(wrong.stage, Stage::Verify);
        assert!(wrong.message.starts_with("checksum mismatch"), "{}", wrong.message);
        let missing = download_and_stage(&release(&server, &body, None), &AtomicBool::new(false), |_| {}).unwrap_err();
        assert_eq!(
            (missing.stage, missing.message.as_str()),
            (Stage::Verify, "the release lists no SHA-256 for BarSQL-darwin-arm64.zip")
        );
        let failed = download_and_stage(&release(&server, &body, None), &AtomicBool::new(true), |_| {}).unwrap_err();
        assert_eq!(failed.stage, Stage::Download);
    }

    #[test]
    fn archives_may_not_escape_or_hold_several_entries() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: Vec<u8>| {
            let path = dir.path().join(name);
            fs::write(&path, bytes).unwrap();
            path
        };
        let escape = write("escape.zip", app_zip(&[("../evil", b"x", 0o644)], &[]));
        assert!(extract_single(&escape).unwrap_err().contains("escapes"));
        let two = write("two.zip", app_zip(&[("a/x", b"x", 0o644), ("b/y", b"y", 0o644)], &[]));
        assert_eq!(extract_single(&two).unwrap_err(), "the archive must hold exactly one top-level entry, it holds 2");
        let link = write("link.zip", app_zip(&[("App/x", b"x", 0o644)], &[("App/passwd", "../../etc/passwd")]));
        assert!(extract_single(&link).unwrap_err().contains("escapes"));
        let plain = write("BarSQL", b"binary".to_vec());
        assert_eq!(extract_single(&plain).unwrap(), plain);
    }

    #[test]
    fn a_tarball_unpacks_its_binary() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
        let mut header = tar::Header::new_gnu();
        header.set_size(6);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, "BarSQL", &b"binary"[..]).unwrap();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        let archive = dir.path().join("BarSQL-linux-amd64.tar.gz");
        fs::write(&archive, bytes).unwrap();
        let binary = extract_single(&archive).unwrap();
        assert_eq!(
            (binary.file_name().unwrap().to_str(), fs::read(&binary).unwrap()),
            (Some("BarSQL"), b"binary".to_vec())
        );
        assert!(!archive.exists(), "the archive gives way to its entry");
    }

    #[test]
    fn a_bare_download_is_staged_as_is_and_made_executable() {
        let server = Server::start();
        let body = b"#!/bin/sh\necho appimage\n";
        let release = release_of(&server, "BarSQL-x86_64.AppImage", body, Some(Sha256::digest(body).to_vec()));
        let staged = download_and_stage(&release, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(staged.path, staged.dir.join("BarSQL-x86_64.AppImage"));
        assert_eq!(fs::read(&staged.path).unwrap(), body);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&staged.path).unwrap().permissions().mode() & 0o777, 0o755);
        }
        fs::remove_dir_all(staged.dir).unwrap();
    }

    fn app(root: &Path, name: &str, text: &str) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(path.join("Contents")).unwrap();
        fs::write(path.join("Contents/version"), text).unwrap();
        path
    }

    fn version(app: &Path) -> String {
        fs::read_to_string(app.join("Contents/version")).unwrap()
    }

    fn job(target: &Path, new: &Path) -> Job {
        Job {
            target: target.into(),
            new: new.into(),
            package: None,
            version: "9.9.9".into(),
            prompt: String::new(),
            parent: 0,
            log: None,
        }
    }

    type Admin = Box<dyn Fn(&[OsString]) -> io::Result<i32>>;

    #[derive(Default)]
    struct Fake {
        running: bool,
        launch_fails: bool,
        admin: Option<Admin>,
        launched: RefCell<Vec<PathBuf>>,
        asked: RefCell<Vec<(PathBuf, Vec<OsString>)>>,
    }

    impl Host for Fake {
        fn exited(&self, _: u32, _: Duration) -> bool {
            !self.running
        }

        fn launch(&self, target: &Path) -> io::Result<()> {
            self.launched.borrow_mut().push(target.into());
            if self.launch_fails { Err(io::Error::other("no")) } else { Ok(()) }
        }

        fn as_admin(&self, program: &Path, args: &[OsString], _: &str) -> io::Result<i32> {
            self.asked.borrow_mut().push((program.into(), args.to_vec()));
            self.admin.as_ref().map_or(Ok(0), |admin| admin(args))
        }
    }

    #[test]
    fn the_helper_swaps_the_app_launches_it_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let target = app(dir.path(), "BarSQL.app", "old");
        let staging = dir.path().join("barsql-update-1-2");
        let new = app(&staging, "BarSQL.app", "new");
        let host = Fake::default();
        assert_eq!(run_helper(&job(&target, &new), &host), 0);
        assert_eq!(version(&target), "new");
        assert_eq!(*host.launched.borrow(), std::slice::from_ref(&target));
        assert!(host.asked.borrow().is_empty(), "a writable folder needs no admin rights");
        assert!(!dir.path().join("BarSQL.app.bak").exists() && !staging.exists(), "backup and staging cleaned up");
    }

    #[test]
    fn a_failed_update_leaves_the_old_version_and_starts_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let target = app(dir.path(), "BarSQL.app", "old");
        let new = app(&dir.path().join("barsql-update-3-4"), "BarSQL.app", "new");
        let running = Fake { running: true, ..Fake::default() };
        assert_eq!(run_helper(&Job { parent: 42, ..job(&target, &new) }, &running), 17, "the app never exited");
        assert_eq!(version(&target), "old");
        assert!(running.launched.borrow().is_empty() && new.exists(), "the staged build stays for another try");

        let missing = Fake::default();
        assert_eq!(run_helper(&job(&target, &dir.path().join("barsql-update-5-6/BarSQL.app")), &missing), 11);
        assert_eq!(*missing.launched.borrow(), std::slice::from_ref(&target));

        let failing = Fake { launch_fails: true, ..Fake::default() };
        assert_eq!(run_helper(&job(&target, &new), &failing), 15);
        assert_eq!(version(&target), "old", "the backup is back");
        assert_eq!(*failing.launched.borrow(), [target.clone(), target.clone()], "the new build, then the old one");
        assert_eq!(run_helper(&job(&dir.path().join("gone.app"), &new), &Fake::default()), 10);
    }

    #[test]
    fn the_helper_claims_its_staging_folder_and_starts_a_fresh_log() {
        let dir = tempfile::tempdir().unwrap();
        let target = app(dir.path(), "BarSQL.app", "old");
        let staging = dir.path().join("barsql-update-3-4");
        let new = app(&staging, "BarSQL.app", "new");
        let log = dir.path().join("update.log");
        fs::write(&log, "the last update\n").unwrap();
        let running = Fake { running: true, ..Fake::default() };
        assert_eq!(run_helper(&Job { parent: 42, log: Some(log.clone()), ..job(&target, &new) }, &running), 17);
        assert!(staging.join(".helper").exists(), "claimed before waiting for the app");
        let text = fs::read_to_string(&log).unwrap();
        assert!(text.contains("helper start") && !text.contains("the last update"), "{text}");

        let gone = dir.path().join("gone.app");
        assert_eq!(admin_swap(&gone, &new, "", &log), 12);
        let text = fs::read_to_string(&log).unwrap();
        assert!(text.contains("helper start") && text.contains("admin swap start"), "the admin half appends: {text}");
        let missing = dir.path().join("no.log");
        assert_eq!(admin_swap(&gone, &new, "", &missing), 12);
        assert!(!missing.exists(), "the admin half never creates a log");
    }

    #[test]
    fn abandoned_staging_folders_and_old_logs_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let folder = |name: &str| {
            let path = dir.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("BarSQL"), "build").unwrap();
            path
        };
        let abandoned = folder("barsql-update-5-100");
        let running = folder("barsql-update-7-100");
        let claimed = folder("barsql-update-8-100");
        let old_log = dir.path().join("barsql-update-5.log");
        fs::write(&old_log, "log").unwrap();
        let others: Vec<PathBuf> =
            ["barsql-update-5", "barsql-update-x-1", "barsql-update-5-1a", "barsql-updater-5-1", "notes-5-1"]
                .into_iter()
                .map(folder)
                .collect();
        let alive = |pid| pid == 7;
        let hours = |hours: f64| SystemTime::now() + Duration::from_secs_f64(hours * 3600.);
        fs::File::create(claimed.join(".helper")).unwrap().set_modified(hours(1.5)).unwrap();

        sweep_staging(dir.path(), hours(0.5), alive);
        assert!(abandoned.exists() && old_log.exists(), "nothing is an hour old yet");

        sweep_staging(dir.path(), hours(2.), alive);
        assert!(!abandoned.exists() && !old_log.exists(), "their app is gone and nothing touched them for an hour");
        assert!(running.exists(), "its app is still running");
        assert!(claimed.exists(), "a helper touched it half an hour ago");
        assert!(others.iter().all(|path| path.exists()), "not named like ours");
    }

    #[cfg(unix)]
    #[test]
    fn a_protected_folder_is_swapped_by_the_admin_half() {
        use std::os::unix::fs::PermissionsExt;

        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let protected = dir.path().join("Applications");
        let target = app(&protected, "BarSQL.app", "old");
        let lock = move |dir: &Path, mode| fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
        let admin_half = {
            let protected = protected.clone();
            move |args: &[OsString]| {
                let [_, target, new, version, log] = args else { panic!("{args:?}") };
                lock(&protected, 0o755);
                let code = admin_swap(Path::new(target), Path::new(new), &version.to_string_lossy(), Path::new(log));
                lock(&protected, 0o555);
                Ok(code)
            }
        };

        let staging = dir.path().join("barsql-update-7-8");
        let new = app(&staging, "BarSQL.app", "new");
        let host = Fake { admin: Some(Box::new(admin_half)), ..Fake::default() };
        lock(&protected, 0o555);
        let code = run_helper(&job(&target, &new), &host);
        lock(&protected, 0o755);
        assert_eq!(code, 0);
        assert_eq!(version(&target), "new");
        let asked = host.asked.borrow();
        let swap: [OsString; 5] =
            ["--barsql-update-swap".into(), target.clone().into(), new.clone().into(), "9.9.9".into(), "".into()];
        assert_eq!(asked[0].1, swap);
        assert_eq!(*host.launched.borrow(), std::slice::from_ref(&target));
        assert!(!protected.join("BarSQL.app.bak").exists() && !staging.exists(), "backup and staging cleaned up");

        let new = app(&dir.path().join("barsql-update-9-10"), "BarSQL.app", "newer");
        let declined =
            Fake { admin: Some(Box::new(|_: &[OsString]| Err(io::Error::other("User canceled.")))), ..Fake::default() };
        lock(&protected, 0o555);
        let code = run_helper(&job(&target, &new), &declined);
        lock(&protected, 0o755);
        assert_eq!(code, 18);
        assert_eq!(version(&target), "new", "nothing changed");
        assert_eq!(*declined.launched.borrow(), std::slice::from_ref(&target), "the old build starts again");
    }

    #[test]
    fn a_package_goes_through_its_package_manager() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("BarSQL");
        fs::write(&target, "old").unwrap();
        let managers = [
            (Kind::Deb, "BarSQL.deb", "/usr/bin/dpkg", &["-i"][..]),
            (Kind::Rpm, "BarSQL.rpm", "/usr/bin/rpm", &["-U"]),
            (Kind::Pacman, "BarSQL.pkg.tar.zst", "/usr/bin/pacman", &["-U", "--noconfirm"]),
        ];
        for (kind, name, manager, flags) in managers {
            let staging = dir.path().join(format!("barsql-update-{name}"));
            fs::create_dir(&staging).unwrap();
            let package = staging.join(name);
            fs::write(&package, "package").unwrap();
            let host = Fake::default();
            assert_eq!(run_helper(&Job { package: Some(kind), ..job(&target, &package) }, &host), 0, "{name}");
            let args: Vec<OsString> = flags.iter().map(OsString::from).chain([package.into()]).collect();
            assert_eq!(*host.asked.borrow(), [(PathBuf::from(manager), args)]);
            assert_eq!(*host.launched.borrow(), std::slice::from_ref(&target));
            assert!(!staging.exists(), "{name} staging cleaned up");
        }

        let staging = dir.path().join("barsql-update-refused");
        fs::create_dir(&staging).unwrap();
        let package = staging.join("BarSQL.deb");
        fs::write(&package, "package").unwrap();
        let refused = Fake { admin: Some(Box::new(|_: &[OsString]| Ok(126))), ..Fake::default() };
        assert_eq!(run_helper(&Job { package: Some(Kind::Deb), ..job(&target, &package) }, &refused), 18);
        assert_eq!(*refused.launched.borrow(), std::slice::from_ref(&target), "the old build starts again");
    }

    #[test]
    fn the_target_is_the_app_bundle_on_macos() {
        let exe = Path::new("/Applications/BarSQL.app/Contents/MacOS/BarSQL");
        let expected = if cfg!(target_os = "macos") { Path::new("/Applications/BarSQL.app") } else { exe };
        assert_eq!(bundle_target(exe), expected);
        assert_eq!(bundle_target(Path::new("/usr/bin/BarSQL")), Path::new("/usr/bin/BarSQL"));
    }

    // Dry run on real artifacts. Point BARSQL_RELEASE_DIR at the package scripts' output plus the
    // SHA256SUMS the release job adds.
    #[test]
    fn a_packaged_release_stages_its_app() {
        let Some(dir) = std::env::var_os("BARSQL_RELEASE_DIR") else { return };
        let server = Server::start();
        let mut assets = Vec::new();
        for entry in fs::read_dir(&dir).unwrap().flatten().filter(|entry| entry.path().is_file()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let body = fs::read(entry.path()).unwrap();
            let url = format!("{}/download/{name}", server.base);
            assets.push(format!(r#"{{"name":"{name}","size":{},"browser_download_url":"{url}"}}"#, body.len()));
            server.route(&format!("/download/{name}"), 200, body);
        }
        let json = format!(r#"{{"tag_name":"v99.0.0","html_url":"","assets":[{}]}}"#, assets.join(","));
        server.route("/repos/Bare7a/BarSQL/releases/latest", 200, json);
        let release =
            check(&server.base, "1.5.3", Kind::Archive, platform(), arch()).unwrap().expect("the release is offered");
        assert!(release.digest.is_some(), "SHA256SUMS lists {}", release.asset.name);
        let staged = download_and_stage(&release, &AtomicBool::new(false), |_| {}).unwrap();
        let expected = match platform() {
            "darwin" => "BarSQL.app",
            "windows" => "BarSQL.exe",
            _ => "BarSQL",
        };
        assert_eq!(staged.path.file_name().unwrap(), expected);
        let binary =
            if platform() == "darwin" { staged.path.join("Contents/MacOS/BarSQL") } else { staged.path.clone() };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(fs::metadata(&binary).unwrap().permissions().mode() & 0o111, 0, "{} runs", binary.display());
        }
        assert!(binary.is_file());
        let _ = fs::remove_dir_all(&staged.dir);
    }
}
