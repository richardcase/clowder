# M4 adapter notifications — Codex / aider / goose

Research date: 2026-07-29. Goal: determine how muxy can detect "needs attention"
(NeedsInput) and "finished" (Completed) for three CLI coding agents it launches in
a terminal, given that muxy can (a) drop a scoped config file in the per-agent
worktree, (b) set env vars, (c) set CLI flags, (d) point a tool at its relay binary
`muxy-hook` (which reads `MUXY_AGENT_ID` + a socket path from env and posts one
event), and (e) fall back to scanning terminal VT signals (BEL / OSC 9 / OSC 777).

Primary sources only (official docs + source repos). Every claim is cited. Where a
detail could not be confirmed from a primary source it is marked **Uncertain —
verify at implementation**.

Payload-delivery summary (matters because `muxy-hook` currently reads env, not a
payload): **Codex** passes a JSON string as the final **argv** argument; **goose**
writes a JSON payload to the hook command's **stdin**; **aider** passes **nothing**
(bare shell invocation). All three differ from muxy-hook's current env-only design.

---

## 1. OpenAI Codex CLI

### Launch command
- Base interactive session: `codex` (no subcommand) launches the interactive TUI.
  Source: Codex CLI reference, "The foundational command to launch the interactive
  terminal UI is `codex` with no subcommand."
  <https://developers.openai.com/codex/cli/reference> (redirects to
  <https://learn.chatgpt.com/docs/developer-commands?surface=cli>)
- Working directory flag: `--cd, -C <path>` "Set the working directory for the agent
  before it starts processing your request." (same reference)
- Inline config override: `-c, --config key=value` — "Override configuration values.
  Values parse as TOML if possible; otherwise the literal string is used." Takes
  precedence over `~/.codex/config.toml` for that invocation. (same reference)

### Config discovery (project-local?)
- Global user config: `~/.codex/config.toml`.
- Project-local config: `.codex/config.toml` inside a project **is** loaded, "only
  when you trust the project."
  Source: config reference — <https://learn.chatgpt.com/docs/config-file/config-reference>
- **Important caveat:** project-scoped config files **cannot** override machine-local
  keys, and the reference explicitly lists **notification** among the keys a project
  config may not set (alongside provider/auth/telemetry). So dropping a `notify` key
  in a worktree `.codex/config.toml` will be **ignored**. (same reference)
  - Consequence for muxy: set `notify` via the **global** config, or (preferred,
    per-agent) via the CLI flag `-c 'notify=["muxy-hook"]'` at launch — the `-c`
    override is user-invoked, not a project-scoped file. **Uncertain — verify at
    implementation** whether `-c` is also blocked for the notification key; the
    documented restriction is worded as applying to *project-scoped config files*,
    not to `-c`, so `-c` is expected to work, but confirm.

### Notification mechanism A — legacy `notify` (Confirmed)
- Config key: `notify` = array of argv strings. Example:
  `notify = ["python3", "/path/to/notify.py"]`.
  Source: config (advanced) — <https://learn.chatgpt.com/docs/config-file/config-advanced>:
  "Use `notify` to trigger an external program whenever Codex emits supported events
  (currently only `agent-turn-complete`)." "The script receives a single JSON
  argument." "This is handy for desktop toasts, chat webhooks, CI updates, or any
  side-channel alerting" — i.e. an **arbitrary** external program is supported, so it
  can be `muxy-hook`.
- Event: **only** `agent-turn-complete` fires it (the model finished its turn / is
  now idle awaiting the user). No other events. (config-advanced, above)
- **Exact payload** — a single JSON string passed as the **final argv arg** to the
  program. Field names (kebab-case) confirmed from source and its test fixture:
  ```json
  {
    "type": "agent-turn-complete",
    "thread-id": "b5f6c1c2-...-444455556666",
    "turn-id": "12345",
    "cwd": "/Users/example/project",
    "client": "codex-tui",
    "input-messages": ["Rename `foo` to `bar` and update the callsites."],
    "last-assistant-message": "Rename complete and verified `cargo build` succeeds."
  }
  ```
  Field names: `type`, `thread-id`, `turn-id`, `cwd`, `client` (optional; omitted
  when absent), `input-messages` (array), `last-assistant-message` (nullable).
  Source (authoritative — the serializer + its "historical wire shape" test):
  <https://github.com/openai/codex/blob/main/codex-rs/hooks/src/legacy_notify.rs>
  (`UserNotification::AgentTurnComplete`, `#[serde(rename_all = "kebab-case")]`,
  and `expected_notification_json()`). The program is spawned via `command_from_argv`
  with the JSON appended as the last arg (`command.arg(notify_payload)`), stdin/out/
  err set to null — so muxy-hook would receive the payload as `argv[last]`, not env
  or stdin.
- Delivery mechanism (argv, not stdin) also confirmed by config-advanced: "The script
  receives a single JSON argument."

### Notification mechanism B — new lifecycle hooks system (partly Uncertain)
- Codex has a newer `[hooks]` lifecycle system, layered across user/project/session/
  managed configs. `docs/config.md` states admins can set
  `allow_managed_hooks_only = true` in `requirements.toml` to ignore "user, project,
  and session hook configs" — i.e. **project- and session-scoped hooks exist**.
  Source: <https://github.com/openai/codex/blob/main/docs/config.md>
- 11 event names (source `HOOK_EVENT_NAMES`): `PreToolUse`, `PermissionRequest`,
  `PostToolUse`, `PreCompact`, `PostCompact`, `SessionStart`, `SessionEnd`,
  `UserPromptSubmit`, `SubagentStart`, `SubagentStop`, `Stop`.
  Source: <https://github.com/openai/codex/blob/main/codex-rs/hooks/src/lib.rs>
  (also mirrored in config-reference). `Stop` corresponds to the same "turn finished"
  moment as legacy notify's `AfterAgent`.
- The `[hooks]` config is documented as matcher groups with command handlers
  ("Command hooks are currently supported; prompt and agent hook handlers are parsed
  but skipped").
  Source: config-reference — <https://learn.chatgpt.com/docs/config-file/config-reference>
- **Uncertain — verify at implementation:** the exact `[hooks]` TOML shape (the key
  names for the command array, matcher, per-event table) and the exact stdin/argv
  payload for `Stop`/`SessionEnd` hooks were NOT fully pinned from primary source in
  this pass. If muxy wants a *project-scoped native* hook (since legacy `notify`
  can't be project-scoped), this is the path — but confirm the TOML schema and
  payload against `codex-rs/hooks/src/events/*` and the config-reference before
  coding against it. Do **not** assume it matches the legacy kebab-case argv payload.

### Event → attention-state mapping (muxy)
- `agent-turn-complete` (legacy notify) / `Stop` (hooks) → **NeedsInput / Completed**
  (turn done, agent idle awaiting user). Codex does not emit a distinct "blocked on
  approval / needs input mid-turn" event through the legacy notify path;
  `PermissionRequest`/`UserPromptSubmit` exist in the hooks system if finer states
  are wanted later.
- No native "Working" event needed; muxy can treat launch/`UserPromptSubmit` as
  Working and the turn-complete event as the transition to NeedsInput.

### Confidence
- Legacy `notify` key, event (`agent-turn-complete`), argv-JSON payload + field
  names, arbitrary-binary support, `codex` launch, `-c`/`--cd` flags, project vs
  global config discovery, and the notification-key-not-project-overridable caveat:
  **Confirmed from primary sources.**
- New `[hooks]` exact TOML schema + payload: **Uncertain — verify at implementation.**
- Whether `-c 'notify=[...]'` is honored despite the project-scope notification
  restriction: **Uncertain — verify at implementation.**

---

## 2. aider

### Launch command
- Base interactive session: `aider` (run in the target directory) starts interactive
  pair-programming chat.
  Source: <https://aider.chat/docs/config/aider_conf.html>

### Config discovery (project-local? — yes)
- aider loads `.aider.conf.yml` from, in order: (1) home directory, (2) git repo
  root, (3) current directory. "Files loaded last will take priority."
  Source: <https://aider.chat/docs/config/aider_conf.html>
  → muxy CAN drop a scoped `.aider.conf.yml` in the worktree (cwd / git root) and it
  wins over the home config. Env vars (`AIDER_*`) and CLI flags also work.

### Notification mechanism (Confirmed; but limited)
- Two options:
  - `--notifications` / `--no-notifications` — boolean, default **False**. "Enable/
    disable terminal bell notifications when LLM responses are ready." Env var
    `AIDER_NOTIFICATIONS`; YAML key `notifications`.
  - `--notifications-command COMMAND` — default **None**. "Specify a command to run
    for notifications instead of the terminal bell. If not specified, a default
    command for your OS may be used." Env var `AIDER_NOTIFICATIONS_COMMAND`; YAML key
    `notifications-command`.
  Source (exact argparse defs): <https://github.com/Aider-AI/aider/blob/main/aider/args.py>
  and <https://aider.chat/docs/config/options.html>,
  <https://aider.chat/docs/config/aider_conf.html>
- **Exact trigger:** fires **only** when "LLM responses are ready" — i.e. the LLM has
  finished and aider is about to wait on the user's input. Implementation: `ring_bell()`
  runs when a `bell_on_next_input` flag (set in `llm_started()`) is true and the next
  input prompt is about to appear. There is **no** separate completion / tool / error
  event — it is a single waiting-for-input signal.
  Source: <https://github.com/Aider-AI/aider/blob/main/aider/io.py> (`ring_bell`,
  `llm_started`, comment "Mark that the LLM has started processing, so we should ring
  the bell on next input").
- **Payload / invocation shape:** the custom command is invoked **bare** — no
  arguments, no payload, no stdin JSON. Confirmed:
  `result = subprocess.run(self.notifications_command, shell=True, capture_output=True)`.
  The human-readable message `"Aider is waiting for your input"` is baked into the
  *default* OS command string only (via `get_default_notification_command()`); a
  user-supplied command receives nothing.
  Source: <https://github.com/Aider-AI/aider/blob/main/aider/io.py>
  → muxy sets `notifications-command: muxy-hook` (or via `AIDER_NOTIFICATIONS_COMMAND`
  / `--notifications-command`). muxy-hook must self-identify from env (`MUXY_AGENT_ID`)
  since aider passes no arguments. muxy must also enable `notifications: true`
  (default is off), or set only the command — **Uncertain — verify at
  implementation:** whether setting `notifications-command` alone is enough, or
  whether `notifications: true` is also required to arm the bell path. Confirm against
  io.py's gating of `ring_bell()`.

### Event → attention-state mapping (muxy)
- notification fired → **NeedsInput** (LLM finished, waiting on the user). This is the
  only native signal; treat "Working" as the interval between a prompt submit and the
  next notification. No native Completed-vs-NeedsInput distinction (they coincide).

### Scoped per-project?
- Yes — `.aider.conf.yml` in the worktree/git-root is loaded and takes priority; env
  var and flag also available. **Confirmed.**

### Confidence
- Flags, env vars, YAML keys, defaults, trigger semantics (waiting-for-input only),
  bare-invocation (no payload), and config discovery: **Confirmed from primary
  sources.** Whether `notifications: true` must accompany `notifications-command`:
  **Uncertain — verify at implementation.**

---

## 3. goose (block/goose)

### Launch command
- Base interactive session: `goose session` (interactive session in the current
  directory); `goose` is the CLI binary with subcommands (`session`, `run`,
  `configure`, etc.). Confirm exact subcommand/args against the CLI at implementation;
  the CLI crate exposes `session`, `run`, `configure`, `project`, `recipe`, `tui`,
  etc. Source (command surface): repo tree
  <https://github.com/block/goose/tree/main/crates/goose-cli/src/commands>
  (`session.rs`, `tui.rs`, `configure.rs`, ...). **Uncertain — verify at
  implementation:** the exact interactive launch invocation (`goose` vs
  `goose session`) and dir flag; confirm via `goose --help`.

### Notification / hook mechanism — goose HAS a hooks system (Confirmed)
Earlier assumption ("no programmatic surface, VT-only") is **wrong** — goose ships a
first-class hooks system (Open Plugins spec).

- Docs: <https://github.com/block/goose/blob/main/documentation/docs/guides/context-engineering/hooks.md>
  (published under block.github.io/goose docs → "Context Engineering ▸ Hooks").
  Source impl: <https://github.com/block/goose/tree/main/crates/goose/src/hooks>
  (`mod.rs`). Example plugin:
  <https://github.com/block/goose/tree/main/examples/plugins/hello-hooks>.
- Hooks live in **plugins** discovered from:
  - User: `~/.agents/plugins/<plugin-name>/`
  - **Project: `<project>/.agents/plugins/<plugin-name>/`** (loaded when goose starts
    from that project) → muxy can drop a scoped plugin in the worktree.
  - Installed-plugin dir.
  Each plugin has `plugin.json` + `hooks/hooks.json` (+ scripts).
- `hooks.json` maps an event name → rules → command handlers:
  ```json
  {
    "hooks": {
      "Stop": [
        { "hooks": [ { "type": "command",
                       "command": "${PLUGIN_ROOT}/scripts/notify.sh" } ] }
      ]
    }
  }
  ```
  Fields: `matcher` (optional regex; omit or use `".*"` to match all — a bare `"*"` is
  invalid regex and silently skips the rule), `hooks` (actions), `type` (`command`,
  default), `command` (run via `sh -c`), `timeout` (seconds, default 30). goose sets
  `PLUGIN_ROOT` in the command env.
- **Payload:** goose writes a **JSON payload to the command's stdin** (NOT argv, NOT
  env). Every payload has `event` + `session_id`; other fields are event-dependent:
  `matcher_context`, `tool_name`, `tool_input`, `message` (on `UserPromptSubmit`),
  `last_assistant_message` (on `Stop` when there is assistant output), `working_dir`
  (tool events). Example `Stop` payload:
  ```json
  { "event": "Stop", "session_id": "abc-123",
    "last_assistant_message": "Done. I updated the file and ran the tests." }
  ```
  → muxy-hook would need to read stdin (and/or rely on `MUXY_AGENT_ID` from env for
  identity), since goose passes no argv payload.
- **Supported events** (from the hooks doc table):
  `SessionStart`, `SessionEnd`, `Stop` ("goose finishes a turn or receives a stop
  event"), `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`,
  `BeforeReadFile`, `AfterFileEdit`, `BeforeShellExecution`, `AfterShellExecution`.
- Only `PreToolUse` and `Stop` can *block*; the rest are observation-only — fine for
  muxy, which only wants to observe.

### Not a notification surface (context)
- goose's `custom_notifications.rs` / `tool_notifications.rs` /
  `notification_events.rs` are **ACP (Agent Client Protocol) JSON-RPC session
  notifications** to a connected ACP client (e.g. the Electron desktop app), plus the
  desktop app's own toasts — **not** an OS-notify/argv hook for the CLI. muxy should
  ignore these and use the hooks system instead. Source:
  <https://github.com/block/goose/blob/main/crates/goose-sdk-types/src/custom_notifications.rs>
  (`_goose/unstable/session/update`: `UsageUpdate` / `StatusMessage` / `MessageUsage`).
- No terminal-bell / OSC-notify config key was found in the goose CLI (no `notify` /
  `bell` config surface located). If muxy does not use the hooks system, it must fall
  back to terminal BEL/OSC scanning. **Uncertain (negative):** could not exhaustively
  prove absence of a bell/OSC key (GitHub code-search API returned no results for this
  token); treat "no CLI notify/bell key" as high-confidence-but-not-exhaustive.

### Event → attention-state mapping (muxy)
- `Stop` → **NeedsInput / Completed** (turn finished; `last_assistant_message`
  present). Primary signal.
- `UserPromptSubmit` → **Working** (user just submitted a prompt).
- `SessionEnd` → session over (agent exited / done).

### Scoped per-project?
- Yes — project plugins under `<worktree>/.agents/plugins/<name>/` load when goose
  starts from that project. **Confirmed.**

### Confidence
- Hooks system existence, plugin locations (incl. project scope), `hooks.json`
  schema, event list, `Stop` semantics, and stdin-JSON payload field names:
  **Confirmed from primary source (hooks.md + repo).**
- Exact interactive launch invocation / dir flag, and the exhaustive absence of any
  CLI bell/OSC key: **Uncertain — verify at implementation.**

---

## Recommended per-tool approach for muxy

| Tool  | Recommended primary | How | Payload delivery | Project-scoped? | VT fallback |
|-------|--------------------|-----|------------------|-----------------|-------------|
| Codex | Native hook (legacy `notify`) | `-c 'notify=["muxy-hook"]'` at launch (project `.codex/config.toml` can't set notification keys), or global config | JSON string as **final argv** | Not via project file — use `-c` flag or global; new `[hooks]` can be project-scoped | Keep as backup |
| aider | Native hook | `.aider.conf.yml` in worktree with `notifications-command: muxy-hook` (+ likely `notifications: true`), or `AIDER_NOTIFICATIONS_COMMAND` | **None** (bare invocation) — muxy-hook self-IDs from env | Yes (`.aider.conf.yml` in cwd/git-root) | Keep as backup |
| goose | Native hook (plugin) | Drop plugin at `<worktree>/.agents/plugins/muxy/` with `hooks.json` mapping `Stop` (and `SessionEnd`) → `muxy-hook` | JSON payload on **stdin** | Yes (project plugin dir) | Only if hooks unused |

All three have a usable native "turn complete / waiting for input" hook — muxy does
**not** have to rely on VT/BEL/OSC scanning for any of them, though VT-fallback
remains a sensible belt-and-suspenders backup (esp. Codex, where the notify key is
awkward to scope per-project).

Note: muxy-hook must handle three different payload-delivery mechanisms —
Codex = argv, goose = stdin, aider = nothing — or muxy can wrap each in a tiny
per-tool shim that normalizes to the env-based `muxy-hook` it has today.

## Top open questions to resolve at implementation time

1. **Codex `-c notify=` scoping:** Is `-c 'notify=["muxy-hook"]'` honored, given that
   *project-scoped config files* cannot set notification keys? (Expected yes, since
   `-c` is user-level, but confirm.) If not, muxy must mutate global `~/.codex/config.toml`
   or use the new `[hooks]` system.
2. **Codex new `[hooks]` schema + payload:** exact TOML shape (command key, matcher,
   per-event table) and the `Stop`/`SessionEnd` payload (stdin vs argv, field names).
   Needed only if muxy wants a truly project-scoped Codex native hook. Verify against
   `codex-rs/hooks/src/events/*` and config-reference. Do not assume it equals the
   legacy kebab-case argv payload.
3. **aider gating:** does `notifications-command` fire on its own, or must
   `notifications: true` also be set? Confirm in `aider/io.py`.
4. **aider default-command clobber:** ensure setting `notifications-command` fully
   replaces the OS default (so no double-notify) — confirm in io.py.
5. **goose launch invocation:** exact interactive command (`goose` vs `goose session`)
   and working-directory handling; confirm via `goose --help`.
6. **goose plugin discovery timing/trust:** confirm a project plugin in
   `<worktree>/.agents/plugins/` is auto-discovered on `goose` start without an
   install/enable/trust step, and that `Stop` fires per-turn in interactive sessions
   (not only in `goose run`).
7. **muxy-hook payload ingestion:** decide whether muxy-hook reads argv (Codex) /
   stdin (goose) / env-only (aider), or whether per-tool shim scripts normalize to
   the current env-based contract.
8. **goose bell/OSC absence:** confirm (via `goose --help` / config docs) there is no
   terminal-bell or OSC-notify config, so the hooks system is genuinely the only
   native surface.
