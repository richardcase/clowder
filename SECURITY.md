# Security Policy

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for a security vulnerability.

Instead, use GitHub's [private vulnerability reporting](../../security/advisories/new) for this
repository (Security tab → Report a vulnerability). This opens a private conversation with the
maintainer and lets us coordinate a fix before any public disclosure.

If you're unable to use GitHub's advisory flow, you can reach the maintainer directly via a
private message on GitHub to [@richardcase](https://github.com/richardcase).

Please include:

- A description of the vulnerability and its potential impact.
- Steps to reproduce, or a proof of concept if you have one.
- Any relevant version/commit information.

We'll acknowledge reports as soon as we can and keep you updated as we work on a fix.

## Scope

clowder runs local agent processes in git worktrees / jj workspaces with attention routing, and
optionally exposes a TCP listener for remote daemon connections (see
[`docs/remote-tls.md`](docs/remote-tls.md)). Reports involving the daemon's socket handling,
worktree provisioning, remote authentication/TLS, or the release/signing pipeline are all in
scope.
