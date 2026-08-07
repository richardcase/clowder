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
  `scripts/build-libghostty.sh` (full Xcode + zig 0.16). `ClowderCore` / `cd macos && swift test` do not.
- **Don't tag by hand:** release tags are created by the manually-dispatched `release.yml`. A plain
  `git tag` fails locally only because of `tag.gpgsign` in the user's global config, not a repo hook.

See `AGENTS.md` (imported above) for the full build/test/runtime reference and repo layout.
