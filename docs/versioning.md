# Versioning & releases

The version lives in one place: the top-level `VERSION` file (currently `0.1.0`).

- **Rust crates** inherit it via `[workspace.package] version` in the root `Cargo.toml`
  (`version.workspace = true` in each crate).
- **The macOS app** (`Clowder.app` Info.plist `CFBundleShortVersionString`/`CFBundleVersion`) reads
  `VERSION` in `scripts/build-app.sh`.

## Bumping the version

```
scripts/set-version.sh 0.2.0     # writes VERSION + the Cargo workspace version + refreshes Cargo.lock
git commit -am "chore: v0.2.0"
git tag v0.2.0
git push && git push --tags      # the v* tag triggers .github/workflows/release.yml
```

## Releasing

Pushing a `vX.Y.Z` tag runs `.github/workflows/release.yml`, which builds the app and publishes a
GitHub Release with the **unsigned** `Clowder.app` (zipped). The tag must match `VERSION` (the workflow
checks this). An unsigned app is Gatekeeper-quarantined on download — right-click → Open (or
`xattr -dr com.apple.quarantine Clowder.app`). Code-signing/notarization + a DMG land in M6c; a Homebrew
cask in M6f.
