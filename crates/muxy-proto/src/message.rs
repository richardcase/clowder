use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct PaneId(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientToDaemon {
    Attach { pane: PaneId },
    Input { pane: PaneId, bytes: Vec<u8> },
    Resize { pane: PaneId, cols: u16, rows: u16 },
    Detach,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DaemonToClient {
    Attached { pane: PaneId, cols: u16, rows: u16 },
    Output { pane: PaneId, bytes: Vec<u8> },
    PaneExited { pane: PaneId, code: Option<i32> },
    AttentionChanged { pane: PaneId, state: AttentionState },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookKind {
    Notification,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookEvent {
    pub agent_id: PaneId,
    pub kind: HookKind,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttentionState {
    Idle,
    Working,
    NeedsInput,
    Completed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_roundtrips_through_postcard() {
        let msg = ClientToDaemon::Input { pane: PaneId(7), bytes: b"ls\n".to_vec() };
        let bytes = postcard::to_stdvec(&msg).unwrap();
        let back: ClientToDaemon = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn daemon_message_roundtrips_through_postcard() {
        let msg = DaemonToClient::Output { pane: PaneId(7), bytes: b"file.txt\n".to_vec() };
        let bytes = postcard::to_stdvec(&msg).unwrap();
        let back: DaemonToClient = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn hook_event_roundtrips() {
        let e = HookEvent { agent_id: PaneId(3), kind: HookKind::Notification };
        let bytes = postcard::to_stdvec(&e).unwrap();
        assert_eq!(e, postcard::from_bytes::<HookEvent>(&bytes).unwrap());
    }

    #[test]
    fn attention_changed_roundtrips() {
        let m = DaemonToClient::AttentionChanged { pane: PaneId(9), state: AttentionState::NeedsInput };
        let bytes = postcard::to_stdvec(&m).unwrap();
        assert_eq!(m, postcard::from_bytes::<DaemonToClient>(&bytes).unwrap());
    }
}
