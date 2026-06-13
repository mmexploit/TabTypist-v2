// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "TabTypist",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "TabTypist", targets: ["TabTypist"]),
    ],
    dependencies: [
        .package(url: "https://github.com/sparkle-project/Sparkle", from: "2.0.0"),
    ],
    targets: [
        .executableTarget(
            name: "TabTypist",
            dependencies: [
                .product(name: "Sparkle", package: "Sparkle"),
            ],
            path: "Sources/TabTypist",
            resources: [
                .process("Resources"),
            ],
            swiftSettings: []
        ),
    ]
)
