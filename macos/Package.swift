// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "MuxyCore",
    products: [
        .library(name: "MuxyCore", targets: ["MuxyCore"]),
    ],
    targets: [
        .target(name: "MuxyCore"),
        .testTarget(name: "MuxyCoreTests", dependencies: ["MuxyCore"]),
    ],
    swiftLanguageModes: [.v5]
)
