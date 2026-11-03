import Combine
import Foundation
import QuartzCore
import RuffleAppleFFI

/// Stage rendering quality level.
public enum RuffleStageQuality: UInt32, CaseIterable, Sendable {
    case low = 0
    case medium = 1
    case high = 2

    public var title: String {
        switch self {
        case .low: return "Low"
        case .medium: return "Med"
        case .high: return "High"
        }
    }
}

/// Snapshot of the player's most recent tick/render durations and frame counters.
public struct RuffleFrameTimings: Sendable {
    /// Duration of the last `tick()` call (AVM2 scripts, timeline, async tasks) in milliseconds.
    public let tickMs: Double
    /// Duration of the last `render()` call (display list walk + Metal submission) in milliseconds.
    public let renderMs: Double
    /// Total SWF timeline frames executed since launch.
    public let framesRun: UInt64
    /// Target SWF frame rate (e.g. `24.0`).
    public let swfFrameRate: Double

    public init(tickMs: Double, renderMs: Double, framesRun: UInt64, swfFrameRate: Double) {
        self.tickMs = tickMs
        self.renderMs = renderMs
        self.framesRun = framesRun
        self.swfFrameRate = swfFrameRate
    }
}

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

    /// Snapshot of the focused editable Flash `TextField`, or `nil` if none is focused.
    public struct FocusedTextState: Equatable {
        public let text: String
        public let isPassword: Bool
        /// Caret position as a UTF-16 offset into `text`.
        public let caret: Int
    }

    public var focusedTextState: FocusedTextState? {
        guard let handle else { return nil }
        var isPassword = false
        var start = 0
        var end = 0
        guard let ptr = ruffle_player_get_focused_text(handle, &isPassword, &start, &end) else {
            return nil
        }
        defer { ruffle_string_free(ptr) }
        return FocusedTextState(text: String(cString: ptr), isPassword: isPassword, caret: end)
    }

    /// Clears all text in the focused Flash `TextField`.
    public func clearFocusedText() {
        guard let handle else { return }
        ruffle_player_clear_focused_text(handle)
    }

    /// Queries the most recent per-frame CPU/GPU timings and frame counters.
    public var frameTimings: RuffleFrameTimings {
        guard let handle else {
            return RuffleFrameTimings(tickMs: 0, renderMs: 0, framesRun: 0, swfFrameRate: 24)
        }
        var tickMs: Double = 0
        var renderMs: Double = 0
        var framesRun: UInt64 = 0
        var swfFps: Double = 24
        ruffle_player_get_frame_timings(handle, &tickMs, &renderMs, &framesRun, &swfFps)
        return RuffleFrameTimings(
            tickMs: tickMs,
            renderMs: renderMs,
            framesRun: framesRun,
            swfFrameRate: swfFps
        )
    }

    /// Stage anti-aliasing and filter quality (`low`, `medium`, `high`).
    public var quality: RuffleStageQuality {
        get {
            guard let handle else { return .high }
            let raw = ruffle_player_get_quality(handle)
            return RuffleStageQuality(rawValue: raw) ?? .high
        }
        set {
            guard let handle else { return }
            ruffle_player_set_quality(handle, newValue.rawValue)
            objectWillChange.send()
        }
    }

    /// Enables or disables Smooth Motion (sub-frame motion interpolation between 24 FPS SWF ticks).
    public var isSmoothMotionEnabled: Bool {
        get {
            guard let handle else { return false }
            return ruffle_player_is_interpolating(handle)
        }
        set {
            guard let handle else { return }
            ruffle_player_set_interpolating(handle, newValue)
            objectWillChange.send()
        }
    }

    /// Runs garbage collection and frees unused bitmap caches and pooled GPU buffers.
    public func releaseMemory() {
        guard let handle else { return }
        ruffle_player_release_memory(handle)
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

