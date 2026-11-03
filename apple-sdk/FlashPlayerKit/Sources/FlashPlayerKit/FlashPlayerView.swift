import SwiftUI

#if canImport(UIKit)
import UIKit

/// A SwiftUI view that embeds `UIFlashPlayerView` to render `.swf` files natively using Metal.
public struct FlashPlayerView: UIViewRepresentable {
    public enum Source: Equatable {
        case fileURL(URL)
        case data(Data, baseURL: URL?)
    }

    private let source: Source
    @Binding private var isPlaying: Bool
    private let onExternalCall: ((_ name: String, _ args: [Any]) -> Void)?

    public init(
        fileURL: URL,
        isPlaying: Binding<Bool> = .constant(true),
        onExternalCall: ((_ name: String, _ args: [Any]) -> Void)? = nil
    ) {
        self.source = .fileURL(fileURL)
        self._isPlaying = isPlaying
        self.onExternalCall = onExternalCall
    }

    public init(
        data: Data,
        baseURL: URL? = Bundle.main.resourceURL,
        isPlaying: Binding<Bool> = .constant(true),
        onExternalCall: ((_ name: String, _ args: [Any]) -> Void)? = nil
    ) {
        self.source = .data(data, baseURL: baseURL)
        self._isPlaying = isPlaying
        self.onExternalCall = onExternalCall
    }

    public func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    public func makeUIView(context: Context) -> UIFlashPlayerView {
        let view = UIFlashPlayerView(frame: .zero)
        view.player?.onExternalCall = onExternalCall
        loadSourceIfNeeded(into: view, coordinator: context.coordinator)
        view.player?.isPlaying = isPlaying
        return view
    }

    public func updateUIView(_ uiView: UIFlashPlayerView, context: Context) {
        uiView.player?.onExternalCall = onExternalCall
        loadSourceIfNeeded(into: uiView, coordinator: context.coordinator)
        if uiView.player?.isPlaying != isPlaying {
            uiView.player?.isPlaying = isPlaying
        }
    }

    private func loadSourceIfNeeded(into view: UIFlashPlayerView, coordinator: Coordinator) {
        guard coordinator.loadedSource != source else { return }
        coordinator.loadedSource = source
        switch source {
        case .fileURL(let url):
            try? view.load(fileURL: url)
        case .data(let data, let baseURL):
            try? view.load(data: data, baseURL: baseURL)
        }
    }

    public final class Coordinator {
        var loadedSource: Source?
    }
}
#endif

