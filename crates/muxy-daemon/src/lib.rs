pub mod agent;
pub mod pane;
pub mod server;
pub mod notify;
pub mod attention;
pub mod control_json;
mod split_tree;
pub use agent::{
    adapter_descriptors, build_adapter, AdapterDescriptor, AgentAdapter, ClaudeAdapter, CodexAdapter,
    SyntheticAdapter,
};
pub use pane::{Pane, PaneCommand};
pub use server::Daemon;
pub use notify::{FakeNotifier, Notifier, OsNotifier};
