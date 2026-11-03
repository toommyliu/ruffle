import Combine
import Foundation
import QuartzCore
import RuffleAppleFFI

public enum RufflePlayerError: Error, LocalizedError {
    case initializationFailed
    case invalidFileURL(URL)
    case loadFailed(String)

    public var errorDescription: String? {
        switch self {
        case .initializationFailed:
            return "Failed to initialize the native Ruffle Metal player."
        case .invalidFileURL(let url):
            return "Expected a valid local file URL, got: \(url)"
        case .loadFailed(let detail):
            return "Failed to load SWF movie: \(detail)"
        }
    }
}

/// Manages a native Ruffle Flash player instance backed by Apple Metal (`CAMetalLayer`) and CoreAudio.
@MainActor
public final class RufflePlayer: ObservableObject {
    private var handle: OpaquePointer?
    private var retainedSelfForCallback: UnsafeMutableRawPointer?

    /// Invoked whenever ActionScript calls `ExternalInterface.call(name, ...args)` (such as `loader.swf`'s `Externalizer`).
    public var onExternalCall: ((_ name: String, _ args: [Any]) -> Void)?

    /// Invoked when Ruffle requests opening (`true`) or closing (`false`) the virtual keyboard (e.g. when a Flash `TextField` gains/loses focus).
    public var onVirtualKeyboardChanged: ((_ isOpen: Bool) -> Void)?

    /// Invoked when the SWF calls `navigateToURL(...)` for an external website URL.
    public var onNavigateURL: ((_ url: URL) -> Void)?

    @Published public private(set) var movieSize: CGSize = .zero

    /// Creates a new player attached to a `CAMetalLayer` (or headless offscreen Metal target if `metalLayer` is `nil`).
    public init(
        metalLayer: CAMetalLayer? = nil,
        drawableSize: CGSize = CGSize(width: 800, height: 600),
        scaleFactor: CGFloat = 2.0,
        storageDirectoryURL: URL? = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first?.appendingPathComponent("RuffleSharedObjects")
    ) throws {
        let layerPtr: UnsafeMutableRawPointer? = metalLayer.map {
            Unmanaged.passUnretained($0).toOpaque()
        }
        let width = UInt32(max(1.0, drawableSize.width.rounded()))
        let height = UInt32(max(1.0, drawableSize.height.rounded()))
        let scale = Double(max(1.0, scaleFactor))

        let createdHandle: OpaquePointer? = if let storagePath = storageDirectoryURL?.path {
            storagePath.withCString { cStorage in
                ruffle_player_create(layerPtr, width, height, scale, cStorage)
            }
        } else {
            ruffle_player_create(layerPtr, width, height, scale, nil)
        }

        guard let validHandle = createdHandle else {
            throw RufflePlayerError.initializationFailed
        }
        self.handle = validHandle

        let selfPtr = Unmanaged.passUnretained(self).toOpaque()
        self.retainedSelfForCallback = selfPtr
        ruffle_player_set_external_call_callback(
            validHandle,
            { userData, namePtr, argsJsonPtr in
                guard let userData, let namePtr else { return }
                let name = String(cString: namePtr)
                let argsJson = argsJsonPtr.map { String(cString: $0) } ?? "[]"
                let parsedArgs: [Any]
                if let data = argsJson.data(using: .utf8),
                   let array = try? JSONSerialization.jsonObject(with: data) as? [Any] {
                    parsedArgs = array
                } else {
                    parsedArgs = []
                }
                let player = Unmanaged<RufflePlayer>.fromOpaque(userData).takeUnretainedValue()
                if Thread.isMainThread {
                    MainActor.assumeIsolated {
                        player.onExternalCall?(name, parsedArgs)
                    }
                } else {
                    DispatchQueue.main.async {
                        player.onExternalCall?(name, parsedArgs)
                    }
                }
            },
            selfPtr
        )

        ruffle_player_set_virtual_keyboard_callback(
            validHandle,
            { userData, isOpen in
                guard let userData else { return }
                let player = Unmanaged<RufflePlayer>.fromOpaque(userData).takeUnretainedValue()
                if Thread.isMainThread {
                    MainActor.assumeIsolated {
                        player.onVirtualKeyboardChanged?(isOpen)
                    }
                } else {
                    DispatchQueue.main.async {
                        player.onVirtualKeyboardChanged?(isOpen)
                    }
                }
            },
            selfPtr
        )

        ruffle_player_set_navigate_url_callback(
            validHandle,
            { userData, urlPtr in
                guard let userData, let urlPtr else { return }
                let urlStr = String(cString: urlPtr)
                guard let url = URL(string: urlStr) else { return }
                let player = Unmanaged<RufflePlayer>.fromOpaque(userData).takeUnretainedValue()
                if Thread.isMainThread {
                    MainActor.assumeIsolated {
                        player.onNavigateURL?(url)
                    }
                } else {
                    DispatchQueue.main.async {
                        player.onNavigateURL?(url)
                    }
                }
            },
            selfPtr
        )
    }

    deinit {
        if let handle {
            ruffle_player_set_external_call_callback(handle, nil, nil)
            ruffle_player_set_virtual_keyboard_callback(handle, nil, nil)
            ruffle_player_set_navigate_url_callback(handle, nil, nil)
            ruffle_player_destroy(handle)
        }
    }

    /// Loads a local `.swf` file from disk.
    /// Any relative or remote `.swf` files requested at runtime by this movie (e.g. `loader.swf`)
    /// are resolved automatically via Ruffle's `ExternalNavigatorBackend`.
    public func load(fileURL: URL) throws {
        guard let handle else { throw RufflePlayerError.initializationFailed }
        guard fileURL.isFileURL else {
            throw RufflePlayerError.invalidFileURL(fileURL)
        }
        let ok = fileURL.path.withCString { cPath in
            ruffle_player_load_swf_path(handle, cPath)
        }
        guard ok else {
            throw RufflePlayerError.loadFailed(fileURL.path)
        }
        refreshMovieSize()
    }

    /// Loads an in-memory `.swf` `Data` buffer, resolving relative/external requests against `baseURL`.
    public func load(data: Data, baseURL: URL? = Bundle.main.resourceURL) throws {
        guard let handle else { throw RufflePlayerError.initializationFailed }
        let baseStr = baseURL?.absoluteString ?? "file:///movie.swf"
        let ok = data.withUnsafeBytes { rawBuffer -> Bool in
            guard let baseAddress = rawBuffer.bindMemory(to: UInt8.self).baseAddress else {
                return false
            }
            return baseStr.withCString { cBase in
                ruffle_player_load_swf_bytes(handle, baseAddress, data.count, cBase)
            }
        }
        guard ok else {
            throw RufflePlayerError.loadFailed("Invalid or empty SWF data (\(data.count) bytes)")
        }
        refreshMovieSize()
    }

    /// Updates the viewport drawable size (in physical pixels) and screen scale factor.
    public func setViewportDimensions(width: UInt32, height: UInt32, scaleFactor: Double) {
        guard let handle else { return }
        ruffle_player_set_viewport_dimensions(handle, max(1, width), max(1, height), max(1.0, scaleFactor))
    }

    /// Polls async loader tasks and advances the SWF timeline by `dtMillis` milliseconds.
    public func tick(dtMillis: Double) {
        guard let handle else { return }
        ruffle_player_tick(handle, dtMillis)
        refreshMovieSizeIfNeeded()
    }

    /// Returns `true` if the stage is dirty and needs to be rendered.
    public var needsRender: Bool {
        guard let handle else { return false }
        return ruffle_player_needs_render(handle)
    }

    /// Renders the current SWF frame into the attached `CAMetalLayer`.
    public func render() {
        guard let handle else { return }
        ruffle_player_render(handle)
    }

    /// Whether the SWF is currently playing.
    public var isPlaying: Bool {
        get {
            guard let handle else { return false }
            return ruffle_player_is_playing(handle)
        }
        set {
            guard let handle else { return }
            ruffle_player_set_is_playing(handle, newValue)
            objectWillChange.send()
        }
    }

    public func play() {
        isPlaying = true
    }

    public func pause() {
        isPlaying = false
    }

    /// Master audio volume (`0.0` to `1.0`).
    public var volume: Float {
        get {
            guard let handle else { return 0.0 }
            return ruffle_player_volume(handle)
        }
        set {
            guard let handle else { return }
            ruffle_player_set_volume(handle, newValue)
            objectWillChange.send()
        }
    }

    /// Sends a touch/mouse press event in physical pixel coordinates.
    public func touchDown(x: Double, y: Double) {
        guard let handle else { return }
        ruffle_player_touch_down(handle, x, y)
    }

    /// Sends a touch/mouse move event in physical pixel coordinates.
    public func touchMove(x: Double, y: Double) {
        guard let handle else { return }
        ruffle_player_touch_move(handle, x, y)
    }

    /// Sends a touch/mouse release event in physical pixel coordinates.
    public func touchUp(x: Double, y: Double) {
        guard let handle else { return }
        ruffle_player_touch_up(handle, x, y)
    }

    /// Sends UTF-8 text input to the focused Flash `TextField`.
    public func insertText(_ text: String) {
        guard let handle, !text.isEmpty else { return }
        text.withCString { cText in
            ruffle_player_text_input(handle, cText)
        }
    }

    /// Sends a Backspace key press and text control event to the focused Flash `TextField`.
    public func deleteBackward() {
        guard let handle else { return }
        ruffle_player_key_backspace(handle)
    }

    /// Sends an Enter/Return key press and text control event to the player.
    public func insertNewline() {
        guard let handle else { return }
        ruffle_player_key_enter(handle)
    }

    /// Returns `true` if an editable Flash `TextField` (`EditText`) currently has stage focus.
    public var isTextFocused: Bool {
        guard let handle else { return false }
        return ruffle_player_is_text_focused(handle)
    }

    /// Invokes a callback registered in ActionScript via `ExternalInterface.addCallback(name, fn)`.
    /// - Parameters:
    ///   - name: The callback name registered by the SWF.
    ///   - arguments: JSON-serializable arguments array.
    /// - Returns: The decoded JSON return value from ActionScript, if any.
    @discardableResult
    public func callExternalInterface(name: String, arguments: [Any] = []) -> Any? {
        guard let handle else { return nil }
        let jsonData = (try? JSONSerialization.data(withJSONObject: arguments)) ?? Data("[]".utf8)
        let jsonString = String(data: jsonData, encoding: .utf8) ?? "[]"

        let resultPtr: UnsafeMutablePointer<CChar>? = name.withCString { cName in
            jsonString.withCString { cArgs in
                ruffle_player_call_external_interface(handle, cName, cArgs)
            }
        }
        guard let resultPtr else { return nil }
        defer { ruffle_string_free(resultPtr) }

        let resultStr = String(cString: resultPtr)
        guard let data = resultStr.data(using: .utf8) else { return nil }
        return try? JSONSerialization.jsonObject(with: data, options: .fragmentsAllowed)
    }

    private func refreshMovieSize() {
        guard let handle else { return }
        let w = CGFloat(ruffle_player_movie_width(handle))
        let h = CGFloat(ruffle_player_movie_height(handle))
        let newSize = CGSize(width: w, height: h)
        if newSize != movieSize {
            movieSize = newSize
        }
    }

    private func refreshMovieSizeIfNeeded() {
        if movieSize == .zero {
            refreshMovieSize()
        }
    }
}

