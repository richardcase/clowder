# Protocol fixtures

Most of `fixtures/*.json` is the wire format for the JSON control protocol shared between the Rust
daemon and the Swift app — the exception is `worktree-names.json`, described separately below.
For the wire-format fixtures, there are two message directions, each with its own fixture check in
each language, because the direction determines which side is authoritative:

- **`ControlEvent`** (daemon → app): Rust *encodes*, Swift *decodes*. The Rust test
  (`crates/clowder-proto/src/control.rs`, `encoder_matches_the_golden_fixtures`) asserts its
  encoder **produces** each event fixture byte-for-byte. The Swift test
  (`macos/Tests/ClowderCoreTests/ModelsTests.swift`, `testDecodesEveryGoldenFixture`) asserts it
  **decodes** each event fixture into the expected value.
- **`ControlRequest`** (app → daemon): Swift *encodes*, Rust *decodes* — the mirror image. The
  Rust test (`request_fixtures_decode_and_roundtrip`) asserts each request fixture **decodes** to
  the expected value and that re-encoding it reproduces the fixture byte-for-byte. The Swift test
  (`testEncodesEveryGoldenRequestFixtureExactly`) asserts its encoder **produces** the fixture.
  That check compares parsed JSON objects rather than raw bytes: `JSONEncoder` does not preserve
  the key order `encode(to:)` calls `container.encode(_:forKey:)` in, so a literal byte
  comparison against a fixture whose key order comes from Rust's field-declaration order isn't
  stable — the object comparison still fails on a renamed, dropped, added, or wrong-valued field,
  it just isn't sensitive to key order.

Changing a shape means changing the fixture, which fails the checks on both sides (in both
directions, as applicable) until both sides agree again.

**Adding a new message type**: give it a fixture, in whichever direction it flows, following the
existing cases in `encoder_matches_the_golden_fixtures` / `request_fixtures_decode_and_roundtrip`
(Rust) and `testDecodesEveryGoldenFixture` / `testEncodesEveryGoldenRequestFixtureExactly`
(Swift). Derive the fixture from the Rust type first — run the Rust assertion before writing the
Swift side, and if they disagree, fix the fixture, not the encoder.

## CLI stdout (Rust encodes, Swift decodes)

A third direction, alongside `ControlEvent` and `ControlRequest`: JSON that `clowder remote …`
subcommands print on **stdout**, for the macOS app's `HostRegistry` (M11b) to shell out to and
decode. There is no control-socket message for these — the whole point of the host registry is
that it works with **no daemon running**, so its wire format has to be a CLI's stdout instead of a
socket frame.

- **`fixtures/remote-host-list.json`** — `clowder remote list --json`'s `{"hosts": [...]}` array,
  one object per host. Encoded by `HostView`/`ListOut` in `crates/clowder-client/src/remote_cli.rs`
  and asserted byte-exact by its `list_output_matches_the_golden_fixture` test. Note what the
  fixture deliberately omits: the bearer token never appears on stdout, only a `hasToken` boolean.
- **`fixtures/remote-probe.json`** — `clowder remote probe <name> --json`'s `{"probe": {...}}`
  object. Encoded by `ProbeView`/`ProbeOut` in the same file, asserted byte-exact by
  `probe_output_matches_the_golden_fixture`.

Both fixtures are Rust-authoritative, the same rule as `ControlEvent`: run the Rust assertion
first, and if Swift's decoder and the fixture disagree, fix the Swift side.

## `fixtures/host-names.json`

Alongside `worktree-names.json` below: a shared table of `{"name": ..., "valid": ...}` cases
checked against both independent implementations of the host-nickname validation rule — Rust's
`validate_name` (`crates/clowder-config/src/hosts.rs`, exercised by
`name_validation_matches_the_shared_fixture`) and, in M11b, Swift's `HostDraft.nameError`. If you
add or change a rule in either validator, add a case here and mirror the rule in the other
implementation — the same convention `worktree-names.json` established.

## `fixtures/worktree-names.json`

Not a wire message — this is a shared table of `{"name": ..., "valid": ...}` cases checked
against BOTH independent implementations of the worktree-name validation rule: Rust's
`validate_workspace_name` (`crates/clowder-workspace/src/lib.rs`, exercised by
`agrees_with_the_shared_name_cases`) and Swift's `NewWorktreeForm.nameError`
(`macos/Sources/ClowderCore/SheetForms.swift`, exercised by `testAgreesWithTheSharedNameCases`).
A JSON array can't carry a comment explaining that convention, which is why it lives here instead:
if you add or change a rule in either validator, add a case to this file and mirror the rule in
the other implementation — otherwise the two can silently drift apart.
