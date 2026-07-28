use crate::{AgentInfo, AttentionState, PaneId};
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct SplitId(pub u64);

#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Axis { Horizontal, Vertical }

#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SplitDirection { Right, Down }

/// A binary split tree for one agent's workspace. Internally tagged on "kind".
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PaneTree {
    Leaf { pane: PaneId },
    Split {
        id: SplitId,
        axis: Axis,
        ratio: f32,
        first: Box<PaneTree>,
        second: Box<PaneTree>,
    },
}

/// GUI/CLI → daemon, over the JSON-lines control socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ControlRequest {
    ListAgents,
    SpawnAgent { project: String, task: String, adapter: String },
    SplitPane { pane: PaneId, direction: SplitDirection },
    ClosePane { pane: PaneId },
    SetSplitRatio { split: SplitId, ratio: f32 },
    GetSplitTree { agent: PaneId },
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
    SplitTreeChanged { agent: PaneId, tree: PaneTree },
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

    #[test]
    fn split_pane_request_json_shape() {
        let r = ControlRequest::SplitPane { pane: PaneId(2), direction: SplitDirection::Right };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""type":"splitPane""#), "{s}");
        assert!(s.contains(r#""pane":2"#), "{s}");
        assert!(s.contains(r#""direction":"right""#), "{s}");
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }

    #[test]
    fn close_setratio_gettree_requests_roundtrip() {
        for r in [
            ControlRequest::ClosePane { pane: PaneId(5) },
            ControlRequest::SetSplitRatio { split: SplitId(3), ratio: 0.4 },
            ControlRequest::GetSplitTree { agent: PaneId(1) },
        ] {
            let s = serde_json::to_string(&r).unwrap();
            assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap(), "{s}");
        }
    }

    #[test]
    fn split_tree_changed_event_nested_roundtrip() {
        let tree = PaneTree::Split {
            id: SplitId(1), axis: Axis::Horizontal, ratio: 0.5,
            first: Box::new(PaneTree::Leaf { pane: PaneId(1) }),
            second: Box::new(PaneTree::Split {
                id: SplitId(2), axis: Axis::Vertical, ratio: 0.3,
                first: Box::new(PaneTree::Leaf { pane: PaneId(2) }),
                second: Box::new(PaneTree::Leaf { pane: PaneId(3) }),
            }),
        };
        let e = ControlEvent::SplitTreeChanged { agent: PaneId(1), tree };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""type":"splitTreeChanged""#), "{s}");
        assert!(s.contains(r#""kind":"leaf""#) && s.contains(r#""kind":"split""#), "{s}");
        assert!(s.contains(r#""axis":"horizontal""#) && s.contains(r#""axis":"vertical""#), "{s}");
        assert!(s.contains(r#""pane":1"#) && s.contains(r#""id":2"#), "bare numbers: {s}");
        assert_eq!(e, serde_json::from_str::<ControlEvent>(&s).unwrap());
    }
}
