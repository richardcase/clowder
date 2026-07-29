use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct PaneId(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientToDaemon {
    Attach { pane: PaneId },
    Input { pane: PaneId, bytes: Vec<u8> },
    Resize { pane: PaneId, cols: u16, rows: u16 },
    Detach,
    ListAgents,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DaemonToClient {
    Attached { pane: PaneId, cols: u16, rows: u16 },
    Output { pane: PaneId, bytes: Vec<u8> },
    PaneExited { pane: PaneId, code: Option<i32> },
    AttentionChanged { pane: PaneId, state: AttentionState },
    AgentList { agents: Vec<AgentInfo> },
    AgentRemoved { pane: PaneId },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookKind {
    Notification,
    Stop,
    /// The agent is actively working (e.g. Claude's UserPromptSubmit / PreToolUse) —
    /// clears a prior NeedsInput/Completed back to Working.
    Active,
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
    Exited,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub pane: PaneId,
    pub project: String,
    pub task: String,
    pub state: AttentionState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterInfo {
    pub id: String,
    pub display_name: String,
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

    #[test]
    fn list_agents_roundtrips() {
        let m = ClientToDaemon::ListAgents;
        let bytes = postcard::to_stdvec(&m).unwrap();
        assert_eq!(m, postcard::from_bytes::<ClientToDaemon>(&bytes).unwrap());
    }

    #[test]
    fn agent_list_roundtrips() {
        let m = DaemonToClient::AgentList {
            agents: vec![AgentInfo {
                pane: PaneId(2),
                project: "muxy".into(),
                task: "task-a".into(),
                state: AttentionState::NeedsInput,
            }],
        };
        let bytes = postcard::to_stdvec(&m).unwrap();
        assert_eq!(m, postcard::from_bytes::<DaemonToClient>(&bytes).unwrap());
    }

    #[test]
    fn attention_exited_roundtrips() {
        let m = DaemonToClient::AttentionChanged { pane: PaneId(1), state: AttentionState::Exited };
        let bytes = postcard::to_stdvec(&m).unwrap();
        assert_eq!(m, postcard::from_bytes::<DaemonToClient>(&bytes).unwrap());
    }

    #[test]
    fn agent_removed_roundtrips() {
        let m = DaemonToClient::AgentRemoved { pane: PaneId(5) };
        let bytes = postcard::to_stdvec(&m).unwrap();
        assert_eq!(m, postcard::from_bytes::<DaemonToClient>(&bytes).unwrap());
    }
}
