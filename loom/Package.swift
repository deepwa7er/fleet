// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Loom",
    platforms: [.macOS(.v14)],
    dependencies: [
        // Filament is a sibling in this monorepo, so it is referenced by path:
        // one checkout builds both, a change to the reconciler and its use here
        // land in the same commit, and there is no revision to keep in sync.
        // This restores the local-path dependency that predated the split, now
        // that the two are versioned together rather than by branch-tracking.
        .package(path: "../filament")
    ],
    targets: [
        .executableTarget(
            name: "Loom",
            dependencies: [
                .product(name: "Filament", package: "filament"),
                .product(name: "FilamentAppKit", package: "filament"),
            ],
            path: "Sources/Loom"
        )
    ]
)
