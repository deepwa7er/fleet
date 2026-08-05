// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Tiler",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "Tiler",
            path: "Sources/Tiler"
        )
    ]
)
