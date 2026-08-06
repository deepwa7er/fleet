// swift-tools-version: 6.1
import PackageDescription

let package = Package(
    name: "Filament",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "Filament", targets: ["Filament"]),
        .executable(name: "filament-demo", targets: ["FilamentDemo"]),
    ],
    targets: [
        .target(name: "Filament"),
        .executableTarget(name: "FilamentDemo", dependencies: ["Filament"]),
        .testTarget(name: "FilamentTests", dependencies: ["Filament"]),
    ]
)
