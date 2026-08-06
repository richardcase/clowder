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
    pub remote_listen: Option<String>,
    pub remote_host: Option<String>,
    pub remote_tls: bool,
    pub remote_token: Option<String>,
    /// Directory that agent worktrees are provisioned under. See `default_worktree_base_from`.
    pub worktree_base: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    sockets: Option<Sockets>,
    pane: Option<PaneCfg>,
    remote: Option<Remote>,
    worktrees: Option<Worktrees>,
}
#[derive(Debug, Default, Deserialize)]
struct Sockets { client: Option<PathBuf>, control: Option<PathBuf>, hook: Option<PathBuf> }
#[derive(Debug, Default, Deserialize)]
struct PaneCfg { backlog_cap: Option<usize>, shell: Option<String>, cols: Option<u16>, rows: Option<u16> }
#[derive(Debug, Default, Deserialize)]
struct Remote { listen: Option<String>, host: Option<String>, tls: Option<bool>, token: Option<String> }
#[derive(Debug, Default, Deserialize)]
struct Worktrees { base: Option<PathBuf> }

impl Config {
    /// Load `$XDG_CONFIG_HOME/clowder/config.toml` (else `$HOME/.config/clowder/config.toml`), then apply
    /// env overrides. A missing/invalid file is non-fatal.
    pub fn load() -> Config {
        let file = config_path().and_then(read_file).unwrap_or_default();
        Config::resolve(file, &|k| std::env::var(k).ok())
    }

    /// Pure resolver (testable): env > file > default. `get_env(key)` yields the env value.
    fn resolve(f: FileConfig, get_env: &dyn Fn(&str) -> Option<String>) -> Config {
        let s = f.sockets.unwrap_or_default();
        let p = f.pane.unwrap_or_default();
        let r = f.remote.unwrap_or_default();
        let w = f.worktrees.unwrap_or_default();

        // Per-user runtime dir for sockets: $XDG_RUNTIME_DIR › $TMPDIR › /tmp (mirrors the daemon's
        // single-instance PID lock dir). Env socket vars still override below.
        let nonempty = |k: &str| get_env(k).filter(|v| !v.is_empty());
        // parses CLOWDER_REMOTE_TLS: "1"/"true" → Some(true), "0"/"false" → Some(false), else None
        let env_bool = |k: &str| {
            get_env(k).and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" => Some(true),
                "0" | "false" => Some(false),
                _ => None,
            })
        };
        let runtime_dir = nonempty("XDG_RUNTIME_DIR")
            .or_else(|| nonempty("TMPDIR"))
            .unwrap_or_else(|| "/tmp".to_string());
        let default_sock = |name: &str| PathBuf::from(&runtime_dir).join("clowder").join(name);

        let path = |env: &str, file: Option<PathBuf>, def: PathBuf| {
            get_env(env).map(PathBuf::from).or(file).unwrap_or(def)
        };
        Config {
            client_sock: path("CLOWDER_SOCK", s.client, default_sock("clowder.sock")),
            control_sock: path("CLOWDER_CONTROL_SOCK", s.control, default_sock("clowder-control.sock")),
            hook_sock: path("CLOWDER_HOOK_SOCK", s.hook, default_sock("clowder-hook.sock")),
            backlog_cap: get_env("CLOWDER_BACKLOG_CAP").and_then(|v| v.parse().ok())
                .or(p.backlog_cap).unwrap_or(DEFAULT_BACKLOG_CAP),
            shell: get_env("SHELL").or(p.shell).unwrap_or_else(|| "/bin/sh".into()),
            default_cols: p.cols.unwrap_or(DEFAULT_COLS),
            default_rows: p.rows.unwrap_or(DEFAULT_ROWS),
            // An empty value from EITHER env or file means "off" — the daemon skips the TCP bind
            // (rather than failing to parse `""` as a socket address at startup).
            remote_listen: nonempty("CLOWDER_LISTEN").or(r.listen.filter(|s| !s.is_empty())),
            remote_host: nonempty("CLOWDER_REMOTE_HOST").or(r.host.filter(|s| !s.is_empty())),
            remote_tls: env_bool("CLOWDER_REMOTE_TLS").unwrap_or(r.tls.unwrap_or(false)),
            remote_token: nonempty("CLOWDER_REMOTE_TOKEN").or(r.token.filter(|s| !s.is_empty())),
            // Empty means "unset" from EITHER source — unlike the socket keys above. An empty base
            // would be a relative path, silently provisioning worktrees into the daemon's cwd.
            worktree_base: nonempty("CLOWDER_WORKTREE_BASE")
                .map(PathBuf::from)
                .or(w.base.filter(|p| !p.as_os_str().is_empty()))
                .unwrap_or_else(|| default_worktree_base_from(get_env)),
        }
    }
}

/// Where agent worktrees are provisioned: `$XDG_DATA_HOME/clowder/worktrees` ›
/// `$HOME/.local/share/clowder/worktrees` › `/tmp/clowder/worktrees`.
///
/// `DATA` rather than `STATE`/`CACHE` because worktrees hold *uncommitted user work*.
///
/// The `/tmp` last resort is a data-loss footgun — macOS periodically purges `/tmp`, which would
/// take unlanded agent work with it. It is kept only for consistency with `remote_state_dir()`, and
/// is unreachable whenever `HOME` is set.
///
/// Pure: the environment arrives via `get_env`, so `resolve` and its tests drive this same code
/// path. `default_worktree_base()` is the wrapper for callers holding no `Config`.
fn default_worktree_base_from(get_env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let base = get_env("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| get_env("HOME").filter(|s| !s.is_empty()).map(|h| format!("{h}/.local/share")))
        .unwrap_or_else(|| "/tmp".to_string());
    PathBuf::from(base).join("clowder").join("worktrees")
}

/// The default worktree base against the real environment, for callers with no `Config`
/// (e.g. `Daemon::new_with`). Same family as `remote_state_dir()`.
pub fn default_worktree_base() -> PathBuf {
    default_worktree_base_from(&|k| std::env::var(k).ok())
}

impl Default for Config {
    fn default() -> Self { Config::resolve(FileConfig::default(), &|_| None) }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() { return Some(PathBuf::from(x).join("clowder").join("config.toml")); }
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config").join("clowder").join("config.toml"))
}

fn read_file(path: PathBuf) -> Option<FileConfig> {
    let text = std::fs::read_to_string(&path).ok()?;
    match toml::from_str(&text) {
        Ok(cfg) => Some(cfg),
        Err(e) => { eprintln!("clowder-config: ignoring invalid {}: {e}", path.display()); None }
    }
}

/// The durable per-user dir holding remote TLS creds: `$XDG_STATE_HOME/clowder` › `$HOME/.local/state/clowder` › `/tmp/clowder`.
pub fn remote_state_dir() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/state")))
        .unwrap_or_else(|| "/tmp".to_string());
    PathBuf::from(base).join("clowder")
}
pub fn remote_cert_path() -> PathBuf { remote_state_dir().join("remote-cert.pem") }
pub fn remote_key_path() -> PathBuf { remote_state_dir().join("remote-key.pem") }
pub fn remote_token_path() -> PathBuf { remote_state_dir().join("remote-token") }

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn no_env(_: &str) -> Option<String> { None }

    #[test]
    fn defaults_when_empty() {
        let c = Config::resolve(FileConfig::default(), &no_env);
        // Per-user default: no XDG_RUNTIME_DIR/TMPDIR in `no_env` → runtime_dir is /tmp.
        assert_eq!(c.client_sock, PathBuf::from("/tmp/clowder/clowder.sock"));
        assert_eq!(c.backlog_cap, 262144);
        assert_eq!(c.shell, "/bin/sh");
        assert_eq!((c.default_cols, c.default_rows), (80, 24));
        // No XDG_DATA_HOME/HOME in `no_env` → the /tmp last resort.
        assert_eq!(c.worktree_base, PathBuf::from("/tmp/clowder/worktrees"));
    }

    #[test]
    fn worktree_base_honors_xdg_data_home_then_home_then_tmp() {
        let xdg = |k: &str| (k == "XDG_DATA_HOME").then(|| "/xdg/data".to_string());
        assert_eq!(
            Config::resolve(FileConfig::default(), &xdg).worktree_base,
            PathBuf::from("/xdg/data/clowder/worktrees")
        );

        // XDG_DATA_HOME wins over HOME when both are set.
        let both = |k: &str| match k {
            "XDG_DATA_HOME" => Some("/xdg/data".to_string()),
            "HOME" => Some("/home/rc".to_string()),
            _ => None,
        };
        assert_eq!(
            Config::resolve(FileConfig::default(), &both).worktree_base,
            PathBuf::from("/xdg/data/clowder/worktrees")
        );

        let home = |k: &str| (k == "HOME").then(|| "/home/rc".to_string());
        assert_eq!(
            Config::resolve(FileConfig::default(), &home).worktree_base,
            PathBuf::from("/home/rc/.local/share/clowder/worktrees")
        );

        assert_eq!(
            Config::resolve(FileConfig::default(), &no_env).worktree_base,
            PathBuf::from("/tmp/clowder/worktrees")
        );
    }

    #[test]
    fn worktree_base_env_over_file_then_default() {
        let f: FileConfig = toml::from_str("[worktrees]\nbase = \"/file/wt\"\n").unwrap();
        let env = |k: &str| (k == "CLOWDER_WORKTREE_BASE").then(|| "/env/wt".to_string());
        assert_eq!(Config::resolve(f, &env).worktree_base, PathBuf::from("/env/wt"));

        // file only — and it wins over the XDG default
        let f2: FileConfig = toml::from_str("[worktrees]\nbase = \"/file/wt\"\n").unwrap();
        let home = |k: &str| (k == "HOME").then(|| "/home/rc".to_string());
        assert_eq!(Config::resolve(f2, &home).worktree_base, PathBuf::from("/file/wt"));
    }

    #[test]
    fn empty_worktree_base_is_treated_as_unset() {
        // An empty base is a RELATIVE path; taking it would provision into the daemon's cwd.
        let env = |k: &str| match k {
            "CLOWDER_WORKTREE_BASE" => Some(String::new()),
            "HOME" => Some("/home/rc".to_string()),
            _ => None,
        };
        assert_eq!(
            Config::resolve(FileConfig::default(), &env).worktree_base,
            PathBuf::from("/home/rc/.local/share/clowder/worktrees")
        );

        let f: FileConfig = toml::from_str("[worktrees]\nbase = \"\"\n").unwrap();
        let home = |k: &str| (k == "HOME").then(|| "/home/rc".to_string());
        assert_eq!(
            Config::resolve(f, &home).worktree_base,
            PathBuf::from("/home/rc/.local/share/clowder/worktrees")
        );
    }

    #[test]
    fn default_socket_dir_honors_xdg_runtime_dir_then_tmpdir() {
        let xdg = |k: &str| if k == "XDG_RUNTIME_DIR" { Some("/run/user/501".into()) } else { None };
        let c = Config::resolve(FileConfig::default(), &xdg);
        assert_eq!(c.client_sock, PathBuf::from("/run/user/501/clowder/clowder.sock"));
        assert_eq!(c.control_sock, PathBuf::from("/run/user/501/clowder/clowder-control.sock"));
        assert_eq!(c.hook_sock, PathBuf::from("/run/user/501/clowder/clowder-hook.sock"));

        let tmp = |k: &str| if k == "TMPDIR" { Some("/var/folders/xy".into()) } else { None };
        let c2 = Config::resolve(FileConfig::default(), &tmp);
        assert_eq!(c2.client_sock, PathBuf::from("/var/folders/xy/clowder/clowder.sock"));
    }

    #[test]
    fn env_socket_overrides_per_user_default() {
        let env = |k: &str| match k {
            "XDG_RUNTIME_DIR" => Some("/run/user/501".into()),
            "CLOWDER_SOCK" => Some("/env/explicit.sock".into()),
            _ => None,
        };
        let c = Config::resolve(FileConfig::default(), &env);
        assert_eq!(c.client_sock, PathBuf::from("/env/explicit.sock")); // env wins over the per-user default
        assert_eq!(c.control_sock, PathBuf::from("/run/user/501/clowder/clowder-control.sock")); // others still per-user
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
        let env = |k: &str| match k { "CLOWDER_SOCK" => Some("/env/c.sock".into()), "CLOWDER_BACKLOG_CAP" => Some("4096".into()), _ => None };
        let c = Config::resolve(f, &env);
        assert_eq!(c.client_sock, PathBuf::from("/env/c.sock")); // env wins over file
        assert_eq!(c.backlog_cap, 4096);
    }

    #[test]
    fn invalid_backlog_env_falls_through_to_file() {
        let f: FileConfig = toml::from_str("[pane]\nbacklog_cap = 1024\n").unwrap();
        let env = |k: &str| if k == "CLOWDER_BACKLOG_CAP" { Some("notanumber".into()) } else { None };
        assert_eq!(Config::resolve(f, &env).backlog_cap, 1024);
    }

    #[test]
    fn remote_listen_env_over_file_then_none() {
        // env wins over file
        let f = FileConfig { remote: Some(Remote { listen: Some("127.0.0.1:1".into()), host: None, ..Default::default() }), ..Default::default() };
        let env = |k: &str| (k == "CLOWDER_LISTEN").then(|| "127.0.0.1:2".to_string());
        assert_eq!(Config::resolve(f, &env).remote_listen.as_deref(), Some("127.0.0.1:2"));

        // file only
        let f2 = FileConfig { remote: Some(Remote { listen: Some("127.0.0.1:3".into()), host: None, ..Default::default() }), ..Default::default() };
        assert_eq!(Config::resolve(f2, &|_| None).remote_listen.as_deref(), Some("127.0.0.1:3"));

        // neither → None (TCP off)
        assert_eq!(Config::resolve(FileConfig::default(), &|_| None).remote_listen, None);

        // an empty file value is "off", not Some("") (which would fail to parse/bind later)
        let f3 = FileConfig { remote: Some(Remote { listen: Some("".into()), host: None, ..Default::default() }), ..Default::default() };
        assert_eq!(Config::resolve(f3, &|_| None).remote_listen, None);
    }

    #[test]
    fn remote_host_env_over_file_then_none() {
        let f = FileConfig { remote: Some(Remote { listen: None, host: Some("h:1".into()), ..Default::default() }), ..Default::default() };
        let env = |k: &str| (k == "CLOWDER_REMOTE_HOST").then(|| "h:2".to_string());
        assert_eq!(Config::resolve(f, &env).remote_host.as_deref(), Some("h:2"));

        let f2 = FileConfig { remote: Some(Remote { listen: None, host: Some("h:3".into()), ..Default::default() }), ..Default::default() };
        assert_eq!(Config::resolve(f2, &|_| None).remote_host.as_deref(), Some("h:3"));

        assert_eq!(Config::resolve(FileConfig::default(), &|_| None).remote_host, None);

        // empty file value is "off"
        let f4 = FileConfig { remote: Some(Remote { listen: None, host: Some("".into()), ..Default::default() }), ..Default::default() };
        assert_eq!(Config::resolve(f4, &|_| None).remote_host, None);
    }

    #[test]
    fn remote_tls_and_token_resolve_env_over_file() {
        let f = FileConfig { remote: Some(Remote {
            listen: None, host: None, tls: Some(true), token: Some("filetok".into()),
        }), ..Default::default() };
        let env = |k: &str| match k { "CLOWDER_REMOTE_TOKEN" => Some("envtok".to_string()), _ => None };
        let c = Config::resolve(f, &env);
        assert!(c.remote_tls);
        assert_eq!(c.remote_token.as_deref(), Some("envtok"));
    }

    #[test]
    fn remote_tls_defaults_false_and_token_none() {
        let c = Config::resolve(FileConfig::default(), &|_| None);
        assert!(!c.remote_tls);
        assert_eq!(c.remote_token, None);
    }
}
