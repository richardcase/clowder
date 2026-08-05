pub mod agent;
pub mod instance;
pub mod logging;
pub mod pane;
pub mod server;
pub mod notify;
pub mod attention;
pub mod control_json;
pub mod remote;
pub mod remote_tls;
pub mod registry;
pub mod store;
mod split_tree;
pub use agent::{
    adapter_descriptors, build_adapter, AdapterDescriptor, AgentAdapter, ClaudeAdapter, CodexAdapter,
    SyntheticAdapter,
};
pub use pane::{Pane, PaneCommand};
pub use server::Daemon;
pub use notify::{FakeNotifier, Notifier, OsNotifier};

/// `CLOWDER_STATE_FILE` is process-global; every test (in any module) that points it at a scratch
/// dir must hold this lock for its full env-var-dependent span, or it races another such test's
/// `set_var`/`remove_var` against a `Registry::default_path()` read elsewhere in the process.
/// `cargo test` runs test fns in parallel by default, so this must be reachable crate-wide.
#[cfg(test)]
pub(crate) static STATE_FILE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
