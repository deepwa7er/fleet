// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Tiler",
    platforms: [.macOS(.v14)],
    dependencies: [
        .package(path: "../filament")
    ],
    targets: [
        .executableTarget(
            name: "Tiler",
            dependencies: [
                .product(name: "Filament", package: "filament"),
                .product(name: "FilamentAppKit", package: "filament"),
            ],
            path: "Sources/Tiler"
        )
    ]
)
