# Versioning & releases

The version lives in one place: the top-level `VERSION` file (currently `0.2.0`).

- **Rust crates** inherit it via `[workspace.package] version` in the root `Cargo.toml`
  (`version.workspace = true` in each crate).
- **The macOS app** (`Clowder.app` Info.plist `CFBundleShortVersionString`/`CFBundleVersion`) reads
  `VERSION` in `scripts/build-app.sh`.

## Bumping the version

```
scripts/set-version.sh 0.2.0     # writes VERSION + the Cargo workspace version + refreshes Cargo.lock
git commit -am "chore: v0.2.0"
git tag -a v0.2.0 -m "v0.2.0"    # annotated (plain `git tag` is rejected in this repo)
git push && git push origin v0.2.0   # the v* tag triggers .github/workflows/release.yml
```

## Releasing

Pushing a `vX.Y.Z` tag runs `.github/workflows/release.yml`, which builds the app and publishes a
GitHub Release. The tag must match `VERSION` (the workflow checks this). When signing is configured it
attaches a **signed + notarized `Clowder-vX.Y.Z-macos.dmg`**; otherwise it falls back to an unsigned
zipped `.app` (Gatekeeper-quarantined on download — right-click → Open, or
`xattr -dr com.apple.quarantine Clowder.app`).

Signing/notarization and its one-time setup (signing material fetched from Doppler over GitHub OIDC — no
secrets stored in GitHub) are documented in [`code-signing.md`](code-signing.md). A Homebrew cask lands
in M6f.
