# Contributing to clowder

Thanks for your interest in contributing.

## Build & test

See [`AGENTS.md`](AGENTS.md) for the full build/test reference (Rust, Swift, and site), repo
layout, runtime model, and known gotchas — it's kept current for both human contributors and AI
coding agents working in this repo.

Quick start:

```sh
source "$HOME/.cargo/env" && cargo test --workspace     # Rust workspace
cd macos && swift test                                  # ClowderCore unit tests
cd site && npm ci && npm run check && npm test           # marketing site
```

## Workflow

- Work on a feature branch and open a pull request into `main` — don't push to `main` directly.
- Keep CI green. `AGENTS.md` documents the required checks and common CI gotchas.
- **Commit messages are [Conventional Commits](https://www.conventionalcommits.org/)** —
  `type(scope): subject`, with `type` one of `feat`, `fix`, `docs`, `test`, `refactor`, `perf`,
  `ci`, `chore`, `build`, `style`, `revert`. The type also drives the released version, so getting
  it right matters. Run `scripts/check-commit-messages.sh` before pushing.
- **Every commit must be signed** — `main`'s branch protection requires it, with no bypass.
- For non-trivial changes, a design spec + implementation plan under `docs/superpowers/` is the
  house style — see existing specs there for the shape.

## Reporting bugs / requesting features

Open a [GitHub issue](../../issues). Include repro steps for bugs, and for feature requests,
describe the problem you're trying to solve rather than only the solution you have in mind.

## Security

Please don't open a public issue for a security vulnerability — see
[`SECURITY.md`](SECURITY.md) instead.
