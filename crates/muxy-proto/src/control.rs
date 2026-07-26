use crate::{AgentInfo, AttentionState, PaneId};
use serde::{Deserialize, Serialize};

/// GUI/CLI → daemon, over the JSON-lines control socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ControlRequest {
    ListAgents,
    SpawnAgent { project: String, task: String, adapter: String },
}

/// daemon → GUI/CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ControlEvent {
    AgentList { agents: Vec<AgentInfo> },
    AttentionChanged { pane: PaneId, state: AttentionState },
    AgentRemoved { pane: PaneId },
    AgentSpawned { pane: PaneId },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_agents_request_json_shape() {
        let r = ControlRequest::ListAgents;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"type":"listAgents"}"#);
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }

    #[test]
    fn spawn_agent_request_json_shape() {
        let r = ControlRequest::SpawnAgent {
            project: "/p".into(),
            task: "t".into(),
            adapter: "shell".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""type":"spawnAgent""#), "{s}");
        assert!(s.contains(r#""adapter":"shell""#), "{s}");
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }

    #[test]
    fn agent_spawned_event_pane_is_bare_number() {
        let e = ControlEvent::AgentSpawned { pane: PaneId(7) };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""type":"agentSpawned""#), "{s}");
        assert!(s.contains(r#""pane":7"#), "PaneId must serialize as a bare number: {s}");
        assert_eq!(e, serde_json::from_str::<ControlEvent>(&s).unwrap());
    }

    #[test]
    fn attention_changed_event_roundtrips() {
        let e = ControlEvent::AttentionChanged { pane: PaneId(3), state: AttentionState::Exited };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""state":"Exited""#), "{s}");
        assert_eq!(e, serde_json::from_str::<ControlEvent>(&s).unwrap());
    }
}
