# CLAUDE.md

@AGENTS.md

## Claude Code notes

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` — rustup is not auto-sourced in
  this environment.
- **Trust the toolchain over the editor:** stale SourceKit "No such module" / "cannot find type"
  diagnostics are common here; believe `cargo`/`swift` CLI output, not the index.
- **Design history + specs** live in `docs/superpowers/` (specs → plans). Follow the spec → plan →
  subagent-driven-execution → PR flow for anything non-trivial; work on a feature branch, not `main`.
- **Anything touching the macOS app** needs the vendored libghostty — build it with
  `scripts/build-libghostty.sh` (full Xcode + zig 0.16). `MuxyCore` / `cd macos && swift test` do not.
- **Tags are annotated:** use `git tag -a vX.Y.Z -m "…"` (plain `git tag` fails).

See `AGENTS.md` (imported above) for the full build/test/runtime reference and repo layout.
