// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "DockMorph",
    platforms: [.macOS(.v14)],
    targets: [
        .target(name: "DockMorphCore"),
        .executableTarget(name: "dockmorph", dependencies: ["DockMorphCore"]),
    ]
)
