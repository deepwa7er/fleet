// swift-tools-version: 6.1
import PackageDescription

let package = Package(
    name: "Filament",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "Filament", targets: ["Filament"]),
        .library(name: "FilamentAppKit", targets: ["FilamentAppKit"]),
        .executable(name: "filament-demo", targets: ["FilamentDemo"]),
    ],
    targets: [
        .target(name: "Filament"),
        // Kept out of the core target so `Filament` itself imports nothing and
        // stays portable — the same reason react-dom is not inside
        // react-reconciler.
        .target(name: "FilamentAppKit", dependencies: ["Filament"]),
        .executableTarget(name: "FilamentDemo", dependencies: ["Filament"]),
        .testTarget(name: "FilamentTests", dependencies: ["Filament"]),
        .testTarget(name: "FilamentAppKitTests", dependencies: ["FilamentAppKit"]),
    ]
)
