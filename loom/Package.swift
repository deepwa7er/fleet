// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Tiler",
    platforms: [.macOS(.v14)],
    dependencies: [
        // Tracked by branch rather than pinned to a tag: Filament is being
        // built alongside this migration, so tagging every change it needs
        // would be pure ceremony. Package.resolved is committed, so builds stay
        // reproducible even though the requirement is a moving one.
        .package(url: "git@github.com:deepwa7er/filament.git", branch: "main")
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
