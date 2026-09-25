use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, Listener, ListenerOptions, Name, Stream, prelude::*,
};

// One process per data folder. A later launch hands its arguments to the first one and exits.
pub enum Instance {
    First(Option<Listener>),
    Later,
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn name(data_dir: &Path) -> std::io::Result<Name<'static>> {
    let id = format!("eu.bare7a.barsql-{:016x}.sock", fnv1a(data_dir.to_string_lossy().as_bytes()));
    if GenericNamespaced::is_supported() {
        id.to_ns_name::<GenericNamespaced>()
    } else {
        std::env::temp_dir().join(id).to_fs_name::<GenericFilePath>()
    }
}

// Make relative paths absolute here, in the working directory they were typed in.
pub fn claim(data_dir: &Path, args: &[String]) -> Instance {
    let Ok(socket) = name(data_dir) else { return Instance::First(None) };
    if let Ok(mut stream) = Stream::connect(socket.clone()) {
        let absolute: Vec<String> = args
            .iter()
            .map(|arg| {
                std::path::absolute(arg).map_or_else(|_| arg.clone(), |path: PathBuf| path.display().to_string())
            })
            .collect();
        let line = serde_json::to_string(&absolute).unwrap_or_default() + "\n";
        if stream.write_all(line.as_bytes()).is_ok() {
            return Instance::Later;
        }
    }
    Instance::First(ListenerOptions::new().name(socket).try_overwrite(true).create_sync().ok())
}

pub fn serve(listener: Listener, on_args: impl Fn(Vec<String>) + Send + 'static) {
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok()
                && let Ok(args) = serde_json::from_str::<Vec<String>>(line.trim())
            {
                on_args(args);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{Instance, claim, serve};

    #[test]
    fn a_later_launch_hands_over_its_arguments() {
        let dir = std::env::temp_dir().join(format!("barsql-single-{}", std::process::id()));
        let Instance::First(Some(listener)) = claim(&dir, &[]) else { panic!("the first launch listens") };
        let (tx, rx) = std::sync::mpsc::channel();
        serve(listener, move |args| {
            let _ = tx.send(args);
        });
        assert!(matches!(claim(&dir, &["notes.db".into()]), Instance::Later));
        let args = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(args[0].ends_with("notes.db") && std::path::Path::new(&args[0]).is_absolute(), "{args:?}");
        let other = std::env::temp_dir().join(format!("barsql-single-other-{}", std::process::id()));
        assert!(matches!(claim(&other, &[]), Instance::First(Some(_))), "another data folder runs on its own");
    }
}
