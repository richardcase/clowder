use muxy_proto::{AttentionState, PaneId};
use std::sync::Mutex;

pub trait Notifier: Send + Sync {
    fn notify(&self, pane: PaneId, state: AttentionState);
}

/// Real desktop notifications. Only fires for states worth interrupting the user.
pub struct OsNotifier;

impl Notifier for OsNotifier {
    fn notify(&self, pane: PaneId, state: AttentionState) {
        let body = match state {
            AttentionState::NeedsInput => "needs your input",
            AttentionState::Completed => "finished",
            AttentionState::Exited => "exited",
            AttentionState::Idle | AttentionState::Working => return, // not interrupt-worthy
        };
        let _ = notify_rust::Notification::new()
            .summary(&format!("muxy · agent {}", pane.0))
            .body(body)
            .show();
    }
}

/// Test double: records calls instead of showing banners.
pub struct FakeNotifier {
    calls: Mutex<Vec<(PaneId, AttentionState)>>,
}

impl FakeNotifier {
    pub fn new() -> Self {
        Self { calls: Mutex::new(Vec::new()) }
    }
    pub fn calls(&self) -> Vec<(PaneId, AttentionState)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Notifier for FakeNotifier {
    fn notify(&self, pane: PaneId, state: AttentionState) {
        self.calls.lock().unwrap().push((pane, state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_notifier_records_calls() {
        let n = FakeNotifier::new();
        n.notify(PaneId(1), AttentionState::NeedsInput);
        n.notify(PaneId(2), AttentionState::Completed);
        assert_eq!(
            n.calls(),
            vec![(PaneId(1), AttentionState::NeedsInput), (PaneId(2), AttentionState::Completed)]
        );
    }
}
