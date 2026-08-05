# Protocol fixtures

`fixtures/*.json` is the wire format for the JSON control protocol shared between the Rust daemon
and the Swift app. There are two message directions, each with its own fixture check in each
language, because the direction determines which side is authoritative:

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
