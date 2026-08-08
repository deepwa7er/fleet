// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Shutter",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "Shutter",
            path: "Sources/Shutter"
        )
    ]
)
