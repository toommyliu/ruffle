import Foundation
import XCTest
@testable import FlashPlayerKit

@MainActor
final class FlashPlayerKitTests: XCTestCase {
    func testLoadLoaderSwfAndTick() throws {
        let player = try RufflePlayer(
            metalLayer: nil,
            drawableSize: CGSize(width: 800, height: 600),
            scaleFactor: 2.0
        )

        let repoRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let loaderSwfURL = repoRoot.appendingPathComponent("loader.swf")

        guard FileManager.default.fileExists(atPath: loaderSwfURL.path) else {
            XCTFail("Expected loader.swf at \(loaderSwfURL.path)")
            return
        }

        var externalCalls: [String] = []
        player.onExternalCall = { name, _ in
            externalCalls.append(name)
        }

        try player.load(fileURL: loaderSwfURL)
        for _ in 0..<10 {
            player.tick(dtMillis: 16.67)
            player.render()
        }

        XCTAssertTrue(player.isPlaying)
        XCTAssertGreaterThan(player.movieSize.width, 0)
        XCTAssertGreaterThan(player.movieSize.height, 0)
    }
}

