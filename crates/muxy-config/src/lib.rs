use serde::Deserialize;
use std::path::PathBuf;

const DEFAULT_BACKLOG_CAP: usize = 256 * 1024;
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

/// Fully-resolved configuration (env > file > default, applied per field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub client_sock: PathBuf,
    pub control_sock: PathBuf,
    pub hook_sock: PathBuf,
    pub backlog_cap: usize,
    pub shell: String,
    pub default_cols: u16,
    pub default_rows: u16,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    sockets: Option<Sockets>,
    pane: Option<PaneCfg>,
}
#[derive(Debug, Default, Deserialize)]
struct Sockets { client: Option<PathBuf>, control: Option<PathBuf>, hook: Option<PathBuf> }
#[derive(Debug, Default, Deserialize)]
struct PaneCfg { backlog_cap: Option<usize>, shell: Option<String>, cols: Option<u16>, rows: Option<u16> }

impl Config {
    /// Load `$XDG_CONFIG_HOME/muxy/config.toml` (else `$HOME/.config/muxy/config.toml`), then apply
    /// env overrides. A missing/invalid file is non-fatal.
    pub fn load() -> Config {
        let file = config_path().and_then(read_file).unwrap_or_default();
        Config::resolve(file, &|k| std::env::var(k).ok())
    }

    /// Pure resolver (testable): env > file > default. `get_env(key)` yields the env value.
    fn resolve(f: FileConfig, get_env: &dyn Fn(&str) -> Option<String>) -> Config {
        let s = f.sockets.unwrap_or_default();
        let p = f.pane.unwrap_or_default();

        // Per-user runtime dir for sockets: $XDG_RUNTIME_DIR › $TMPDIR › /tmp (mirrors the daemon's
        // single-instance PID lock dir). Env socket vars still override below.
        let nonempty = |k: &str| get_env(k).filter(|v| !v.is_empty());
        let runtime_dir = nonempty("XDG_RUNTIME_DIR")
            .or_else(|| nonempty("TMPDIR"))
            .unwrap_or_else(|| "/tmp".to_string());
        let default_sock = |name: &str| PathBuf::from(&runtime_dir).join("muxy").join(name);

        let path = |env: &str, file: Option<PathBuf>, def: PathBuf| {
            get_env(env).map(PathBuf::from).or(file).unwrap_or(def)
        };
        Config {
            client_sock: path("MUXY_SOCK", s.client, default_sock("muxy.sock")),
            control_sock: path("MUXY_CONTROL_SOCK", s.control, default_sock("muxy-control.sock")),
            hook_sock: path("MUXY_HOOK_SOCK", s.hook, default_sock("muxy-hook.sock")),
            backlog_cap: get_env("MUXY_BACKLOG_CAP").and_then(|v| v.parse().ok())
                .or(p.backlog_cap).unwrap_or(DEFAULT_BACKLOG_CAP),
            shell: get_env("SHELL").or(p.shell).unwrap_or_else(|| "/bin/sh".into()),
            default_cols: p.cols.unwrap_or(DEFAULT_COLS),
            default_rows: p.rows.unwrap_or(DEFAULT_ROWS),
        }
    }
}

impl Default for Config {
    fn default() -> Self { Config::resolve(FileConfig::default(), &|_| None) }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() { return Some(PathBuf::from(x).join("muxy").join("config.toml")); }
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config").join("muxy").join("config.toml"))
}

fn read_file(path: PathBuf) -> Option<FileConfig> {
    let text = std::fs::read_to_string(&path).ok()?;
    match toml::from_str(&text) {
        Ok(cfg) => Some(cfg),
        Err(e) => { eprintln!("muxy-config: ignoring invalid {}: {e}", path.display()); None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn no_env(_: &str) -> Option<String> { None }

    #[test]
    fn defaults_when_empty() {
        let c = Config::resolve(FileConfig::default(), &no_env);
        // Per-user default: no XDG_RUNTIME_DIR/TMPDIR in `no_env` → runtime_dir is /tmp.
        assert_eq!(c.client_sock, PathBuf::from("/tmp/muxy/muxy.sock"));
        assert_eq!(c.backlog_cap, 262144);
        assert_eq!(c.shell, "/bin/sh");
        assert_eq!((c.default_cols, c.default_rows), (80, 24));
    }

    #[test]
    fn default_socket_dir_honors_xdg_runtime_dir_then_tmpdir() {
        let xdg = |k: &str| if k == "XDG_RUNTIME_DIR" { Some("/run/user/501".into()) } else { None };
        let c = Config::resolve(FileConfig::default(), &xdg);
        assert_eq!(c.client_sock, PathBuf::from("/run/user/501/muxy/muxy.sock"));
        assert_eq!(c.control_sock, PathBuf::from("/run/user/501/muxy/muxy-control.sock"));
        assert_eq!(c.hook_sock, PathBuf::from("/run/user/501/muxy/muxy-hook.sock"));

        let tmp = |k: &str| if k == "TMPDIR" { Some("/var/folders/xy".into()) } else { None };
        let c2 = Config::resolve(FileConfig::default(), &tmp);
        assert_eq!(c2.client_sock, PathBuf::from("/var/folders/xy/muxy/muxy.sock"));
    }

    #[test]
    fn env_socket_overrides_per_user_default() {
        let env = |k: &str| match k {
            "XDG_RUNTIME_DIR" => Some("/run/user/501".into()),
            "MUXY_SOCK" => Some("/env/explicit.sock".into()),
            _ => None,
        };
        let c = Config::resolve(FileConfig::default(), &env);
        assert_eq!(c.client_sock, PathBuf::from("/env/explicit.sock")); // env wins over the per-user default
        assert_eq!(c.control_sock, PathBuf::from("/run/user/501/muxy/muxy-control.sock")); // others still per-user
    }

    #[test]
    fn file_overrides_default() {
        let f: FileConfig = toml::from_str(
            "[sockets]\nclient = \"/run/c.sock\"\n[pane]\nbacklog_cap = 1024\ncols = 120\n",
        ).unwrap();
        let c = Config::resolve(f, &no_env);
        assert_eq!(c.client_sock, PathBuf::from("/run/c.sock"));
        assert_eq!(c.backlog_cap, 1024);
        assert_eq!(c.default_cols, 120);
        assert_eq!(c.default_rows, 24); // unspecified → default
    }

    #[test]
    fn env_overrides_file() {
        let f: FileConfig = toml::from_str("[sockets]\nclient = \"/run/c.sock\"\n[pane]\nbacklog_cap = 1024\n").unwrap();
        let env = |k: &str| match k { "MUXY_SOCK" => Some("/env/c.sock".into()), "MUXY_BACKLOG_CAP" => Some("4096".into()), _ => None };
        let c = Config::resolve(f, &env);
        assert_eq!(c.client_sock, PathBuf::from("/env/c.sock")); // env wins over file
        assert_eq!(c.backlog_cap, 4096);
    }

    #[test]
    fn invalid_backlog_env_falls_through_to_file() {
        let f: FileConfig = toml::from_str("[pane]\nbacklog_cap = 1024\n").unwrap();
        let env = |k: &str| if k == "MUXY_BACKLOG_CAP" { Some("notanumber".into()) } else { None };
        assert_eq!(Config::resolve(f, &env).backlog_cap, 1024);
    }
}
