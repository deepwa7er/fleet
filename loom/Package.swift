// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Carousel",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "Carousel",
            path: "Sources/Carousel"
        )
    ]
)
