// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "FlashPlayerKit",
    platforms: [
        .iOS(.v15),
        .macOS(.v13),
    ],
    products: [
        .library(
            name: "FlashPlayerKit",
            targets: ["FlashPlayerKit"]
        ),
    ],
    targets: [
        .binaryTarget(
            name: "RuffleAppleFFI",
            path: "../RuffleApple.xcframework"
        ),
        .target(
            name: "FlashPlayerKit",
            dependencies: ["RuffleAppleFFI"],
            linkerSettings: [
                .linkedFramework("Metal"),
                .linkedFramework("QuartzCore"),
                .linkedFramework("CoreAudio"),
                .linkedFramework("AudioToolbox"),
                .linkedFramework("CoreGraphics"),
                .linkedFramework("Foundation"),
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("UIKit", .when(platforms: [.iOS])),
                .linkedFramework("AppKit", .when(platforms: [.macOS])),
            ]
        ),
        .testTarget(
            name: "FlashPlayerKitTests",
            dependencies: ["FlashPlayerKit"]
        ),
    ]
)

