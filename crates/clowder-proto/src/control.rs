use crate::{AdapterInfo, AttentionState, PaneId, WorktreeInfo};
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

/// One registered project. `name` is derived at the wire boundary (the path's last component)
/// and is not stored — the daemon's record holds only the canonical path and the kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    /// Canonical path to the project root — the identity.
    pub path: String,
    /// Display name: the path's last component.
    pub name: String,
    /// `"git"` or `"jj"`.
    pub kind: String,
}

/// GUI/CLI → daemon, over the JSON-lines control socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ControlRequest {
    ListWorktrees,
    ListAdapters,
    SpawnAgent { project: String, name: String, adapter: String },
    SplitPane { pane: PaneId, direction: SplitDirection },
    ClosePane { pane: PaneId },
    SetSplitRatio { split: SplitId, ratio: f32 },
    GetSplitTree { agent: PaneId },
    LandAgent { pane: PaneId },
    DiscardAgent { pane: PaneId },
    ListProjects,
    AddProject { path: String },
    RemoveProject { path: String },
    OpenProjectTerminal { path: String },
    RestartWorktree { pane: PaneId },
}

/// daemon → GUI/CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ControlEvent {
    WorktreeList { worktrees: Vec<WorktreeInfo> },
    AdapterList { adapters: Vec<AdapterInfo> },
    AttentionChanged { pane: PaneId, state: AttentionState },
    AgentRemoved { pane: PaneId },
    AgentSpawned { pane: PaneId },
    Error { message: String },
    SplitTreeChanged { agent: PaneId, tree: PaneTree },
    ProjectList { projects: Vec<ProjectInfo> },
    ProjectAdded { project: ProjectInfo },
    ProjectRemoved { path: String },
    ProjectTerminalOpened { path: String, pane: PaneId },
    /// The terminal's root pane went away — the user closed it or the shell exited. Clients
    /// drop their `path -> pane` mapping so the next select respawns.
    ProjectTerminalClosed { path: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_list_event_json_shape() {
        let ev = ControlEvent::WorktreeList {
            worktrees: vec![WorktreeInfo {
                pane: PaneId(2),
                project: "/Users/x/code/clowder".into(),
                name: "task-a".into(),
                branch: "clowder/task-a".into(),
                state: AttentionState::Working,
            }],
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains(r#""type":"worktreeList""#), "{s}");
        assert!(s.contains(r#""pane":2"#), "pane must be a bare number: {s}");
        assert!(s.contains(r#""branch":"clowder/task-a""#), "{s}");
        assert_eq!(ev, serde_json::from_str::<ControlEvent>(&s).unwrap());
    }

    #[test]
    fn list_worktrees_request_json_shape() {
        let r = ControlRequest::ListWorktrees;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"type":"listWorktrees"}"#);
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }

    #[test]
    fn spawn_agent_request_json_shape() {
        let r = ControlRequest::SpawnAgent {
            project: "/p".into(),
            name: "t".into(),
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
    fn land_discard_requests_roundtrip() {
        for r in [
            ControlRequest::LandAgent { pane: PaneId(3) },
            ControlRequest::DiscardAgent { pane: PaneId(4) },
        ] {
            let s = serde_json::to_string(&r).unwrap();
            assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap(), "{s}");
        }
        assert!(serde_json::to_string(&ControlRequest::LandAgent { pane: PaneId(3) }).unwrap()
            .contains(r#""type":"landAgent""#));
    }

    #[test]
    fn list_adapters_request_round_trips() {
        let r = ControlRequest::ListAdapters;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"type":"listAdapters"}"#);
        assert_eq!(serde_json::from_str::<ControlRequest>(&s).unwrap(), r);
    }

    #[test]
    fn adapter_list_event_round_trips_with_camelcase() {
        let ev = ControlEvent::AdapterList {
            adapters: vec![AdapterInfo { id: "codex".into(), display_name: "OpenAI Codex".into() }],
        };
        let s = serde_json::to_string(&ev).unwrap();
        // type tag camelCase; struct field display_name → displayName.
        assert_eq!(
            s,
            r#"{"type":"adapterList","adapters":[{"id":"codex","displayName":"OpenAI Codex"}]}"#
        );
        assert_eq!(serde_json::from_str::<ControlEvent>(&s).unwrap(), ev);
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

    #[test]
    fn project_requests_round_trip_with_camel_case_types() {
        for (r, tag) in [
            (ControlRequest::ListProjects, "listProjects"),
            (ControlRequest::AddProject { path: "/p".into() }, "addProject"),
            (ControlRequest::RemoveProject { path: "/p".into() }, "removeProject"),
            (ControlRequest::OpenProjectTerminal { path: "/p".into() }, "openProjectTerminal"),
            (ControlRequest::RestartWorktree { pane: PaneId(4) }, "restartWorktree"),
        ] {
            let s = serde_json::to_string(&r).unwrap();
            assert!(s.contains(&format!(r#""type":"{tag}""#)), "{s}");
            assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
        }
    }

    #[test]
    fn project_events_round_trip_with_camel_case_types() {
        let p = ProjectInfo { path: "/Users/x/code/clowder".into(), name: "clowder".into(), kind: "git".into() };
        for (e, tag) in [
            (ControlEvent::ProjectList { projects: vec![p.clone()] }, "projectList"),
            (ControlEvent::ProjectAdded { project: p.clone() }, "projectAdded"),
            (ControlEvent::ProjectRemoved { path: "/p".into() }, "projectRemoved"),
            (ControlEvent::ProjectTerminalOpened { path: "/p".into(), pane: PaneId(9) }, "projectTerminalOpened"),
            (ControlEvent::ProjectTerminalClosed { path: "/p".into() }, "projectTerminalClosed"),
        ] {
            let s = serde_json::to_string(&e).unwrap();
            assert!(s.contains(&format!(r#""type":"{tag}""#)), "{s}");
            assert_eq!(e, serde_json::from_str::<ControlEvent>(&s).unwrap());
        }
    }

    #[test]
    fn project_terminal_opened_pane_is_a_bare_number() {
        let e = ControlEvent::ProjectTerminalOpened { path: "/p".into(), pane: PaneId(9) };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""pane":9"#), "PaneId must serialize as a bare number: {s}");
    }

    #[test]
    fn spawn_agent_uses_name_not_task() {
        let r = ControlRequest::SpawnAgent {
            project: "/p".into(), name: "add-projects".into(), adapter: "claude".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""name":"add-projects""#), "{s}");
        assert!(!s.contains(r#""task""#), "the field is `name` now: {s}");
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }
}
