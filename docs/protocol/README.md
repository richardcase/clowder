# Protocol fixtures

`fixtures/*.json` is the wire format for the JSON control protocol shared between the Rust daemon
and the Swift app. The Rust test (`crates/clowder-proto/src/control.rs`,
`encoder_matches_the_golden_fixtures`) asserts its encoder **produces** each file byte-for-byte.
The Swift test (`macos/Tests/ClowderCoreTests/ModelsTests.swift`,
`testDecodesEveryGoldenFixture`) asserts it **decodes** each file. Changing a shape means changing
the fixture, which fails both suites until both sides agree again.
