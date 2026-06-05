// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "HebIsland",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "hebisland", targets: ["HebIsland"])
    ],
    dependencies: [],
    targets: [
        .executableTarget(
            name: "HebIsland",
            dependencies: [],
            path: "Sources/HebIsland",
            resources: [.process("Resources")]
        ),
        .testTarget(
            name: "HebIslandTests",
            dependencies: ["HebIsland"],
            path: "Tests/HebIslandTests"
        )
    ]
)
