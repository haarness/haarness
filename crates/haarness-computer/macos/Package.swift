// swift-tools-version: 5.9
import PackageDescription

// haarness-mac: the native half of computer use. The Haarness server starts it when an agent first looks at or acts
// on this Mac and exchanges one JSON object per line with it over stdin and stdout. MacKit holds the logic that needs
// no window server (geometry, key combinations, outline text) so it can be unit tested anywhere Swift runs.
let package = Package(
    name: "HaarnessMac",
    platforms: [.macOS(.v12)],
    products: [
        .executable(name: "haarness-mac", targets: ["MacHelper"]),
    ],
    targets: [
        .target(name: "MacKit"),
        .executableTarget(name: "MacHelper", dependencies: ["MacKit"]),
        .testTarget(name: "MacKitTests", dependencies: ["MacKit"]),
    ]
)
