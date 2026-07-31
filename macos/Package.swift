// swift-tools-version:6.0
import PackageDescription
import Foundation

// The vendored libghostty static lib (built via the M0c-2 spike recipe) lives at a
// gitignored, absolute path under the package. libghostty targets macOS 13.
let pkgDir = URL(fileURLWithPath: #filePath).deletingLastPathComponent().path
let ghosttyLib = "\(pkgDir)/vendor/libghostty/ghostty-internal.a"

let package = Package(
    name: "Clowder",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .library(name: "ClowderCore", targets: ["ClowderCore"]),
        .executable(name: "clowder-app", targets: ["ClowderApp"]),
    ],
    targets: [
        .target(name: "ClowderCore"),
        .testTarget(name: "ClowderCoreTests", dependencies: ["ClowderCore"]),
        // Wraps the real ghostty.h so Swift imports the C structs directly (no
        // hand-written FFI — ABI drift would corrupt silently).
        .systemLibrary(name: "GhosttyKit", path: "Sources/GhosttyKit"),
        .executableTarget(
            name: "ClowderApp",
            dependencies: ["ClowderCore", "GhosttyKit"],
            linkerSettings: [
                .unsafeFlags([ghosttyLib, "-lc++"]),
                .linkedFramework("Metal"),
                .linkedFramework("MetalKit"),
                .linkedFramework("QuartzCore"),
                .linkedFramework("CoreGraphics"),
                .linkedFramework("CoreText"),
                .linkedFramework("CoreVideo"),
                .linkedFramework("AppKit"),
                .linkedFramework("Foundation"),
                .linkedFramework("Carbon"),
                .linkedFramework("IOSurface"),
                .linkedFramework("UniformTypeIdentifiers"),
            ]
        ),
    ],
    swiftLanguageModes: [.v5]
)
