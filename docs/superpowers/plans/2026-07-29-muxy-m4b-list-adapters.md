# muxy M4b — `ListAdapters` Protocol + `list_adapters()`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the daemon's adapter registry **discoverable** — a `ListAdapters` request →
`AdapterList` event carrying `AdapterInfo { id, display_name }`, sourced from the M4a
registry (`adapter_descriptors()`), so M4c's client picker can populate itself.

**Architecture:** `muxy-proto` gains `AdapterInfo` + `ControlRequest::ListAdapters` +
`ControlEvent::AdapterList`, mirroring `AgentInfo`/`ListAgents`/`AgentList` exactly. The
daemon gains `Daemon::list_adapters()` (maps `adapter_descriptors()` → `AdapterInfo`) and a
`control_json` match arm. **One task**: adding the `ControlRequest` variant makes the
daemon's exhaustive `control_json` match non-exhaustive, so proto + daemon must land in one
commit to keep the workspace compiling (they are not independently reviewable).

**Tech Stack:** Rust, serde, tokio. `muxy-proto` (`message.rs`, `control.rs`), `muxy-daemon`
(`server.rs`, `control_json.rs`). Spec:
`docs/superpowers/specs/2026-07-29-muxy-m4-codex-adapter-design.md` (§Adapter registry + protocol).

## Global Constraints

- Wire format mirrors the existing control protocol: `#[serde(tag="type", rename_all="camelCase")]`
  on the enums; `AdapterInfo` needs `#[serde(rename_all="camelCase")]` so `display_name`
  serializes as **`displayName`** (M4c's Swift side decodes that).
- **`list_adapters()` returns the registry DESCRIPTOR ids** (`claude`/`codex`/`shell`) via
  `adapter_descriptors()` — NOT `adapter.id()` (which reports `"synthetic"` for the shell
  entry). This is the M4a final-review carry-forward.
- No client (`macos/`) change — that's M4c. No behavior change to spawning/agents.
- The whole workspace must build and test green at the single commit (no intermediate
  non-compiling state): `source "$HOME/.cargo/env" && cargo test` (and `-p muxy-proto` /
  `-p muxy-daemon`).
- `adapter_descriptors()` / `AdapterDescriptor` already exist in `muxy-daemon::agent` (M4a,
  on main) and are exported from `muxy-daemon/src/lib.rs`.

---

## Task 1: `AdapterInfo` + `ListAdapters`/`AdapterList` + `list_adapters()`

**Files:**
- Modify: `crates/muxy-proto/src/message.rs` (add `AdapterInfo`)
- Modify: `crates/muxy-proto/src/lib.rs` (re-export `AdapterInfo`)
- Modify: `crates/muxy-proto/src/control.rs` (add request + event variants, import `AdapterInfo`, add tests)
- Modify: `crates/muxy-daemon/src/server.rs` (add `Daemon::list_adapters()`)
- Modify: `crates/muxy-daemon/src/control_json.rs` (handle `ListAdapters`, add a test)

**Interfaces:**
- Produces (proto): `AdapterInfo { id: String, display_name: String }`,
  `ControlRequest::ListAdapters`, `ControlEvent::AdapterList { adapters: Vec<AdapterInfo> }`.
- Produces (daemon): `Daemon::list_adapters() -> Vec<muxy_proto::AdapterInfo>`.
- Consumes: `muxy_daemon::adapter_descriptors()` / `AdapterDescriptor` (M4a).

- [ ] **Step 1: Write the failing proto tests** in `crates/muxy-proto/src/control.rs`'s
`#[cfg(test)] mod tests` (mirror the existing `ListAgents` round-trip test):

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-proto`
Expected: FAIL (compile) — `AdapterInfo`, `ListAdapters`, `AdapterList` don't exist.

- [ ] **Step 3: Add `AdapterInfo`** in `crates/muxy-proto/src/message.rs` (next to `AgentInfo`,
same derives + the camelCase attr):

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterInfo {
    pub id: String,
    pub display_name: String,
}
```

- [ ] **Step 4: Re-export it** from `crates/muxy-proto/src/lib.rs` — add `AdapterInfo` to the
`pub use message::{ ... }` list (alongside `AgentInfo`).

- [ ] **Step 5: Add the protocol variants** in `crates/muxy-proto/src/control.rs`:
- import: change the top `use crate::{AgentInfo, AttentionState, PaneId};` to also bring in
  `AdapterInfo`.
- `ControlRequest` (after `ListAgents`): `ListAdapters,`
- `ControlEvent` (after `AgentList`): `AdapterList { adapters: Vec<AdapterInfo> },`

- [ ] **Step 6: Run the proto tests (green)**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-proto`
Expected: PASS.

- [ ] **Step 7: Write the failing daemon test** in `crates/muxy-daemon/src/control_json.rs`'s
test module. Mirror the existing "send a request over the control stream, read the event"
integration test; a `ListAdapters` request must yield an `AdapterList` containing codex.
(Find the existing control-stream test that sends a `ControlRequest` and reads a
`ControlEvent` — copy its client/stream boilerplate, change the request to `ListAdapters`
and assert on the `AdapterList`.) Also add a direct unit assertion on `list_adapters()`:

```rust
    #[test]
    fn list_adapters_returns_registry_descriptor_ids() {
        let daemon = Daemon::new();
        let ids: Vec<String> = daemon.list_adapters().into_iter().map(|a| a.id).collect();
        // Descriptor ids (NOT adapter.id() — shell's adapter reports "synthetic").
        assert!(ids.contains(&"claude".to_string()));
        assert!(ids.contains(&"codex".to_string()));
        assert!(ids.contains(&"shell".to_string()));
        assert!(!ids.contains(&"synthetic".to_string()), "must expose descriptor id 'shell', not 'synthetic'");
    }
```
(`Daemon::new()` is used by existing non-agent tests — no temp repo needed for `list_adapters`.)

- [ ] **Step 8: Run to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon list_adapters`
Expected: FAIL (compile) — `list_adapters` doesn't exist / the `ListAdapters` arm is missing.

- [ ] **Step 9: Add `Daemon::list_adapters()`** in `crates/muxy-daemon/src/server.rs` (next to
`list_agents`). Source it from the registry descriptors:

```rust
    /// The adapters a client may spawn (registry descriptor ids + labels).
    pub fn list_adapters(&self) -> Vec<muxy_proto::AdapterInfo> {
        crate::adapter_descriptors()
            .iter()
            .map(|d| muxy_proto::AdapterInfo { id: d.id.to_string(), display_name: d.display_name.to_string() })
            .collect()
    }
```
(`adapter_descriptors` is exported from `muxy-daemon`'s crate root — reference it as
`crate::adapter_descriptors()`.)

- [ ] **Step 10: Handle the request** in `crates/muxy-daemon/src/control_json.rs` — add a match
arm alongside `ListAgents` (the request loop that builds an `ev: ControlEvent`):

```rust
                                Ok(ControlRequest::ListAdapters) =>
                                    ControlEvent::AdapterList { adapters: self.list_adapters() },
```
(No auto-snapshot on connect is required — the client requests `ListAdapters` explicitly in
M4c. Keep the connect-time snapshot as the existing `AgentList` only.)

- [ ] **Step 11: Run to verify all green**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon` then whole-workspace
`source "$HOME/.cargo/env" && cargo test`.
Expected: PASS — new proto + daemon tests and all existing; the workspace compiles at this
commit (proto variant + its daemon arm added together).

- [ ] **Step 12: Commit**

```bash
git add crates/muxy-proto/src/message.rs crates/muxy-proto/src/lib.rs crates/muxy-proto/src/control.rs \
        crates/muxy-daemon/src/server.rs crates/muxy-daemon/src/control_json.rs
git commit -m "feat(proto,daemon): ListAdapters/AdapterList + list_adapters() from the registry"
```

---

## Final verification

- `source "$HOME/.cargo/env" && cargo test` (whole workspace) → green, including the
  `listAdapters`/`adapterList` round-trips (camelCase `displayName` pinned) and
  `list_adapters()` returning descriptor ids `claude`/`codex`/`shell` (never `synthetic`).
- No `macos/` change (M4c wires the client picker next). No behavior change to spawn/agents.
- End state: a control client can send `{"type":"listAdapters"}` and receive
  `{"type":"adapterList","adapters":[{"id":...,"displayName":...},...]}` — the seam M4c's
  SpawnSheet `Picker` consumes.
