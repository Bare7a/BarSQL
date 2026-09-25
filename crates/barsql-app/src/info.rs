use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub author: String,
    pub email: String,
    pub website: String,
    pub repository: String,
    pub description: String,
}

pub fn app_info() -> AppInfo {
    AppInfo {
        name: "BarSQL".into(),
        version: crate::VERSION.into(),
        author: "Bare7a".into(),
        email: "bare7a@gmail.com".into(),
        website: "https://barsql.bare7a.eu".into(),
        repository: "https://github.com/Bare7a/BarSQL".into(),
        description: "A fast, native SQL client built with Rust and GPUI.".into(),
    }
}

// Example paths use the host's own style rather than one platform's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDefaults {
    pub platform: String,
    pub separator: String,
    // Empty when the home directory cannot be resolved.
    pub ssh_key: String,
    pub ssh_known_hosts: String,
}

pub fn path_defaults() -> PathDefaults {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let mut defaults = PathDefaults {
        platform: platform.into(),
        separator: std::path::MAIN_SEPARATOR.to_string(),
        ..Default::default()
    };
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|h| !h.is_empty());
    if let Some(home) = home.map(std::path::PathBuf::from) {
        defaults.ssh_key = home.join(".ssh").join("id_ed25519").display().to_string();
        defaults.ssh_known_hosts = home.join(".ssh").join("known_hosts").display().to_string();
    }
    defaults
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_reports_the_build_version() {
        let info = app_info();
        assert_eq!((info.name.as_str(), info.version.as_str()), ("BarSQL", crate::VERSION));
    }

    #[test]
    fn path_defaults_match_the_host() {
        let defaults = path_defaults();
        let platform = if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(windows) {
            "windows"
        } else {
            "linux"
        };
        assert_eq!(defaults.platform, platform);
        assert_eq!(defaults.separator, std::path::MAIN_SEPARATOR.to_string());
        if defaults.ssh_key.is_empty() {
            return;
        }
        let home =
            std::path::PathBuf::from(std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap());
        assert_eq!(defaults.ssh_key, home.join(".ssh").join("id_ed25519").display().to_string());
        assert_eq!(defaults.ssh_known_hosts, home.join(".ssh").join("known_hosts").display().to_string());
        let foreign = if std::path::MAIN_SEPARATOR == '/' { '\\' } else { '/' };
        for path in [&defaults.ssh_key, &defaults.ssh_known_hosts] {
            assert!(path.contains(std::path::MAIN_SEPARATOR) && !path.contains(foreign), "{path}");
        }
        assert!(defaults.ssh_key.ends_with("id_ed25519") && defaults.ssh_known_hosts.ends_with("known_hosts"));
    }
}
