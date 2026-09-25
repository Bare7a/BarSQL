use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

// Windows only. Explorer and GPUI both load the icon as resource 1, and GPUI embeds the manifest itself.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = root.join("../../packaging/windows/icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numbers = ["MAJOR", "MINOR", "PATCH"].map(|part| env::var(format!("CARGO_PKG_VERSION_{part}")).unwrap());
    let numeric = format!("{},0", numbers.join(","));
    let mut rc = String::new();
    let quoted = |path: &PathBuf| path.display().to_string().replace('\\', "\\\\");
    writeln!(rc, "1 ICON \"{}\"", quoted(&icon)).unwrap();
    writeln!(rc, "1 VERSIONINFO\nFILEVERSION {numeric}\nPRODUCTVERSION {numeric}\nFILEOS 0x40004\nFILETYPE 0x1")
        .unwrap();
    rc.push_str("BEGIN\n  BLOCK \"StringFileInfo\"\n  BEGIN\n    BLOCK \"040904b0\"\n    BEGIN\n");
    for (key, value) in [
        ("CompanyName", "Bare7a"),
        ("FileDescription", "BarSQL"),
        ("FileVersion", &version),
        ("InternalName", "BarSQL"),
        ("LegalCopyright", "(c) 2026, Bare7a"),
        ("OriginalFilename", "BarSQL.exe"),
        ("ProductName", "BarSQL"),
        ("ProductVersion", &version),
        ("Comments", "A fast, native SQL client built with Rust and GPUI."),
    ] {
        writeln!(rc, "      VALUE \"{key}\", \"{value}\"").unwrap();
    }
    rc.push_str(
        "    END\n  END\n  BLOCK \"VarFileInfo\"\n  BEGIN\n    VALUE \"Translation\", 0x409, 1200\n  END\nEND\n",
    );
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("barsql.rc");
    fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}
