// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "TabTypist",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "TabTypist", targets: ["TabTypist"]),
    ],
    targets: [
        .executableTarget(
            name: "TabTypist",
            path: "Sources/TabTypist",
            resources: [
                .process("Resources"),
            ],
            swiftSettings: []
        ),
    ]
)
