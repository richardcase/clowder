# Building libghostty

`macos/Sources/GhosttyKit/include/ghostty.h` is committed; the 189 MB
`macos/vendor/libghostty/ghostty-internal.a` is gitignored and built from source by
`scripts/build-libghostty.sh`.

## Requirements

- **zig 0.16.0** (ghostty's `minimum_zig_version`): `brew install zig` (or the matching release).
- **Full Xcode** (not just Command Line Tools) — the Metal shader compiler (`xcrun metal`) is
  Xcode-only and the ghostty renderer needs it:
  ```
  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
  sudo xcodebuild -license accept
  xcrun metal --version   # must succeed
  ```

## Build

```
scripts/build-libghostty.sh
```

This clones ghostty at the pin `2de5e7d38e1354759211722a8687c0815d2cf02c` into `.cache/ghostty`
(gitignored), applies `scripts/libghostty-darwin-install.patch` (adds the macOS static-lib install
that ghostty's `build.zig` otherwise guards off), runs
`SDKROOT="$(xcrun --show-sdk-path)" zig build -Dapp-runtime=none -Demit-xcframework=false`, and copies
`ghostty-internal.a` + `ghostty.h` into the repo. The build is heavy (Metal shaders + ~189 MB, several
minutes); the ghostty clone is cached for cheap re-runs.

## Bumping the pin

Change `GHOSTTY_PIN` in `scripts/build-libghostty.sh`, re-run it, rebuild the app
(`cd macos && swift build`), and commit the regenerated `ghostty.h` (the `.a` stays gitignored). If
ghostty's `build.zig` static-lib block moved, regenerate `scripts/libghostty-darwin-install.patch`.
