//! Application storage paths. Provider credentials keep their own locations.

use std::ffi::OsString;
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    resolve_data_dir(|name| std::env::var_os(name))
}

/// Variables a rebranded build (Heron) sets for itself at startup so it runs
/// beside Zeron on its own data dir and engine port. The build supplies the
/// defaults (`ZERON_BUILD_DATA_DIR_NAME`, under `$HOME`, and
/// `ZERON_BUILD_IPC_PORT`); explicitly set variables are left alone.
pub fn build_profile_env(
    env: impl Fn(&str) -> Option<OsString>,
    data_dir_name: Option<&str>,
    ipc_port: Option<&str>,
) -> Vec<(&'static str, OsString)> {
    let mut vars = vec![];
    if let Some(name) = data_dir_name
        && env("ZERON_DATA_DIR").is_none()
        && let Some(home) = env("HOME")
    {
        vars.push(("ZERON_DATA_DIR", PathBuf::from(home).join(name).into()));
    }
    if let Some(port) = ipc_port
        && env("ZERON_IPC_PORT").is_none()
    {
        vars.push(("ZERON_IPC_PORT", port.into()));
    }
    vars
}

fn resolve_data_dir(mut env: impl FnMut(&str) -> Option<OsString>) -> PathBuf {
    if let Some(dir) = env("ZERON_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        // Explorer does not set HOME. Do not let a shell-specific HOME select
        // a different workspace from a desktop launch, or migrate credentials
        // between Unix-style and native Windows directories implicitly.
        let local = env("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env("USERPROFILE")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join("AppData").join("Local"))
            })
            .expect("LOCALAPPDATA and USERPROFILE not set; set ZERON_DATA_DIR");
        local.join("Zeron")
    }
    #[cfg(not(windows))]
    {
        let home = PathBuf::from(env("HOME").expect("HOME not set"));
        let dir = home.join(".zeron");
        // One-shot 0.2.0 migration: adopt the pre-rename data dir.
        if !dir.exists() {
            let old = home.join(".comet-native");
            if old.exists() && std::fs::rename(&old, &dir).is_ok() {
                eprintln!("migrated data dir {} -> {}", old.display(), dir.display());
            }
        }
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ways it fails: a Heron build still opens ~/.zeron or port 27654 (the
    /// engine lock then refuses it beside Zeron, or its UI attaches to Zeron's
    /// engine); a stock build gains a default it never had; an explicit
    /// ZERON_DATA_DIR / ZERON_IPC_PORT is overridden.
    #[test]
    fn a_rebranded_build_defaults_to_its_own_profile() {
        let env = |vars: &[(&'static str, &'static str)]| {
            let vars = vars.to_vec();
            move |name: &str| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        let home = [("HOME", "/Users/k")];
        assert!(build_profile_env(env(&home), None, None).is_empty());
        assert_eq!(
            build_profile_env(env(&home), Some(".heron"), Some("27664")),
            vec![
                ("ZERON_DATA_DIR", OsString::from("/Users/k/.heron")),
                ("ZERON_IPC_PORT", OsString::from("27664")),
            ]
        );
        let explicit = [
            ("HOME", "/Users/k"),
            ("ZERON_DATA_DIR", "/elsewhere"),
            ("ZERON_IPC_PORT", "1"),
        ];
        assert!(build_profile_env(env(&explicit), Some(".heron"), Some("27664")).is_empty());
    }

    fn resolve(vars: &[(&str, &str)]) -> PathBuf {
        resolve_data_dir(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.into())
        })
    }

    #[test]
    fn explicit_data_dir_needs_no_home() {
        assert_eq!(
            resolve(&[("ZERON_DATA_DIR", "custom data")]),
            PathBuf::from("custom data")
        );
    }

    #[cfg(windows)]
    #[test]
    fn explorer_launch_without_home_uses_local_app_data() {
        assert_eq!(
            resolve(&[("LOCALAPPDATA", r"C:\Users\Test User\AppData\Local")]),
            PathBuf::from(r"C:\Users\Test User\AppData\Local\Zeron"),
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_profile_fallback_handles_unicode_and_apostrophes() {
        assert_eq!(
            resolve(&[("USERPROFILE", r"C:\Users\O'Brien 日本語")]),
            PathBuf::from(r"C:\Users\O'Brien 日本語\AppData\Local\Zeron"),
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_does_not_depend_on_shell_home() {
        assert_eq!(
            resolve(&[("HOME", r"D:\msys-home"), ("LOCALAPPDATA", r"C:\Local")]),
            PathBuf::from(r"C:\Local\Zeron"),
        );
    }
}
