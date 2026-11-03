import QuartzCore

#if canImport(UIKit)
import UIKit

/// A native iOS `UIView` backed by a `CAMetalLayer` that renders SWF movies via Ruffle's Metal renderer
/// and supports native iOS touch and virtual keyboard (`UIKeyInput`) interaction.
@MainActor
public final class UIFlashPlayerView: UIView, UIKeyInput, UITextInputTraits {
    override public class var layerClass: AnyClass {
        CAMetalLayer.self
    }

    public var metalLayer: CAMetalLayer {
        // Guaranteed by `layerClass` above
        layer as! CAMetalLayer
    }

    /// The underlying `RufflePlayer` instance driving this view.
    public private(set) var player: RufflePlayer?

    /// Multiplier applied to the screen's native scale factor (`0.5` to `1.0`).
    /// Lowering this reduces drawable resolution on high-DPI Retina screens to save GPU bandwidth.
    public var renderScale: CGFloat = 1.0 {
        didSet {
            let clamped = max(0.25, min(1.0, renderScale))
            if renderScale != clamped {
                renderScale = clamped
                return
            }
            syncDrawableSizeIfNeeded()
        }
    }

    /// Rolling display render frames per second (updated once per second).
    public private(set) var currentRenderFPS: Double = 0.0
    /// Rolling SWF logical frames per second (updated once per second).
    public private(set) var currentSwfFPS: Double = 0.0

    private var displayLink: CADisplayLink?
    private var lastTimestamp: CFTimeInterval?
    private var lastDrawableSize: CGSize = .zero
    private var lastScaleFactor: CGFloat = 0.0
    private var isAppActive: Bool = true
    private var fpsSampleStartTimestamp: CFTimeInterval = 0
    private var fpsSampleRenderCount: Int = 0
    private var fpsSampleStartSwfFrames: UInt64 = 0

    // MARK: - UITextInputTraits & UIKeyInput

    public var autocorrectionType: UITextAutocorrectionType = .no
    public var autocapitalizationType: UITextAutocapitalizationType = .none
    public var spellCheckingType: UITextSpellCheckingType = .no
    public var smartQuotesType: UITextSmartQuotesType = .no
    public var smartDashesType: UITextSmartDashesType = .no
    public var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
    public var keyboardType: UIKeyboardType = .asciiCapable
    public var returnKeyType: UIReturnKeyType = .done

    override public var canBecomeFirstResponder: Bool {
        true
    }

    public var hasText: Bool {
        true
    }

    private lazy var keyboardBar: KeyboardInputAccessoryView = {
        let bar = KeyboardInputAccessoryView(frame: .zero)
        bar.onClear = { [weak self] in
            self?.player?.clearFocusedText()
            self?.renderIfPossible()
            self?.refreshKeyboardBar()
        }
        bar.onPaste = { [weak self] in
            guard let text = UIPasteboard.general.string, !text.isEmpty else { return }
            self?.insertText(text)
        }
        bar.onSend = { [weak self] in
            self?.insertText("\n")
        }
        bar.onDismiss = { [weak self] in
            _ = self?.resignFirstResponder()
        }
        return bar
    }()

    override public var inputAccessoryView: UIView? {
        keyboardBar
    }

    private func refreshKeyboardBar() {
        guard isFirstResponder else { return }
        keyboardBar.update(state: player?.focusedTextState)
    }

    override public func becomeFirstResponder() -> Bool {
        let result = super.becomeFirstResponder()
        if result {
            // This can be invoked from inside Ruffle's keyboard callback while the player lock is held,
            // so never query the player synchronously here (the lock is not reentrant).
            DispatchQueue.main.async { [weak self] in
                guard let self, self.isFirstResponder else { return }
                self.keyboardBar.update(state: self.player?.focusedTextState, force: true)
            }
        }
        return result
    }

    public func insertText(_ text: String) {
        guard let player else { return }
        if text == "\n" {
            player.insertNewline()
            renderIfPossible()
            _ = resignFirstResponder()
        } else {
            player.insertText(text)
            renderIfPossible()
            refreshKeyboardBar()
        }
    }

    public func deleteBackward() {
        guard let player else { return }
        player.deleteBackward()
        renderIfPossible()
        refreshKeyboardBar()
    }

    // MARK: - Initialization

    override public init(frame: CGRect) {
        super.init(frame: frame)
        commonInit()
    }

    public required init?(coder: NSCoder) {
        super.init(coder: coder)
        commonInit()
    }

    /// Convenience initializer that creates the view and loads a local `.swf` file URL.
    public convenience init(frame: CGRect = .zero, fileURL: URL) throws {
        self.init(frame: frame)
        try load(fileURL: fileURL)
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    private var effectiveScaleFactor: CGFloat {
        let screenScale = window?.screen.scale ?? UIScreen.main.scale
        return max(1.0, screenScale * renderScale)
    }

    private var canRender: Bool {
        window != nil
            && !isHidden
            && isAppActive
            && UIApplication.shared.applicationState == .active
            && bounds.width > 0
            && bounds.height > 0
    }

    private func commonInit() {
        isMultipleTouchEnabled = false
        isUserInteractionEnabled = true

        let scale = effectiveScaleFactor
        contentScaleFactor = scale

        let metalLayer = self.metalLayer
        metalLayer.contentsScale = scale
        metalLayer.isOpaque = true

        let initialWidth = max(1.0, (bounds.width * scale).rounded())
        let initialHeight = max(1.0, (bounds.height * scale).rounded())
        let initialDrawableSize = CGSize(width: initialWidth, height: initialHeight)
        metalLayer.drawableSize = initialDrawableSize
        lastDrawableSize = initialDrawableSize
        lastScaleFactor = scale

        self.player = try? RufflePlayer(
            metalLayer: metalLayer,
            drawableSize: initialDrawableSize,
            scaleFactor: scale
        )

        self.player?.onVirtualKeyboardChanged = { [weak self] isOpen in
            guard let self else { return }
            if isOpen {
                if !self.isFirstResponder {
                    _ = self.becomeFirstResponder()
                }
            } else {
                if self.isFirstResponder {
                    _ = self.resignFirstResponder()
                }
            }
        }

        self.player?.onNavigateURL = { url in
            if UIApplication.shared.canOpenURL(url) {
                UIApplication.shared.open(url)
            }
        }

        registerLifecycleObservers()
    }

    private func registerLifecycleObservers() {
        let center = NotificationCenter.default
        center.addObserver(
            self,
            selector: #selector(appWillResignActive),
            name: UIApplication.willResignActiveNotification,
            object: nil
        )
        center.addObserver(
            self,
            selector: #selector(appDidEnterBackground),
            name: UIApplication.didEnterBackgroundNotification,
            object: nil
        )
        center.addObserver(
            self,
            selector: #selector(appDidBecomeActive),
            name: UIApplication.didBecomeActiveNotification,
            object: nil
        )
        center.addObserver(
            self,
            selector: #selector(appDidReceiveMemoryWarning),
            name: UIApplication.didReceiveMemoryWarningNotification,
            object: nil
        )
    }

    @objc private func appDidReceiveMemoryWarning() {
        player?.releaseMemory()
    }

    @objc private func appWillResignActive() {
        isAppActive = false
        lastTimestamp = nil
        displayLink?.isPaused = true
    }

    @objc private func appDidEnterBackground() {
        isAppActive = false
        lastTimestamp = nil
        displayLink?.isPaused = true
    }

    @objc private func appDidBecomeActive() {
        isAppActive = true
        lastTimestamp = nil
        displayLink?.isPaused = false
        syncDrawableSizeIfNeeded()
        renderIfPossible()
    }

    /// Loads a local `.swf` file into the view's player.
    public func load(fileURL: URL) throws {
        syncDrawableSizeIfNeeded()
        try player?.load(fileURL: fileURL)
        renderIfPossible()
    }

    /// Loads an in-memory `.swf` `Data` buffer into the view's player.
    public func load(data: Data, baseURL: URL? = Bundle.main.resourceURL) throws {
        syncDrawableSizeIfNeeded()
        try player?.load(data: data, baseURL: baseURL)
        renderIfPossible()
    }

    override public func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil {
            isAppActive = UIApplication.shared.applicationState == .active
            syncDrawableSizeIfNeeded()
            startDisplayLink()
            renderIfPossible()
        } else {
            stopDisplayLink()
        }
    }

    override public func layoutSubviews() {
        super.layoutSubviews()
        syncDrawableSizeIfNeeded()
    }

    private func syncDrawableSizeIfNeeded() {
        let scale = effectiveScaleFactor
        if contentScaleFactor != scale {
            contentScaleFactor = scale
        }
        if metalLayer.contentsScale != scale {
            metalLayer.contentsScale = scale
        }

        let drawableWidth = max(1.0, (bounds.width * scale).rounded())
        let drawableHeight = max(1.0, (bounds.height * scale).rounded())
        let newDrawableSize = CGSize(width: drawableWidth, height: drawableHeight)

        if newDrawableSize != lastDrawableSize || scale != lastScaleFactor {
            lastDrawableSize = newDrawableSize
            lastScaleFactor = scale
            metalLayer.drawableSize = newDrawableSize
            player?.setViewportDimensions(
                width: UInt32(drawableWidth),
                height: UInt32(drawableHeight),
                scaleFactor: Double(scale)
            )
            renderIfPossible()
        }
    }

    private func renderIfPossible() {
        guard canRender, let player else { return }
        player.render()
    }

    private func startDisplayLink() {
        guard displayLink == nil else { return }
        lastTimestamp = nil
        let proxy = DisplayLinkTarget { [weak self] link in
            self?.onDisplayLink(link)
        }
        let link = CADisplayLink(target: proxy, selector: #selector(DisplayLinkTarget.step(_:)))
        link.isPaused = !isAppActive
        link.add(to: .main, forMode: .common)
        displayLink = link
    }

    private func stopDisplayLink() {
        displayLink?.invalidate()
        displayLink = nil
        lastTimestamp = nil
    }

    private func onDisplayLink(_ link: CADisplayLink) {
        guard canRender, let player else { return }
        let now = link.timestamp
        let dtSeconds: CFTimeInterval
        if let last = lastTimestamp {
            dtSeconds = max(0.0, min(0.25, now - last))
        } else {
            dtSeconds = link.duration > 0 ? link.duration : (1.0 / 60.0)
        }
        lastTimestamp = now

        player.tick(dtMillis: dtSeconds * 1000.0)
        refreshKeyboardBar()
        if player.needsRender {
            player.render()
            fpsSampleRenderCount += 1
        }

        if fpsSampleStartTimestamp == 0 {
            fpsSampleStartTimestamp = now
            fpsSampleStartSwfFrames = player.frameTimings.framesRun
        } else {
            let elapsed = now - fpsSampleStartTimestamp
            if elapsed >= 0.5 {
                let timings = player.frameTimings
                let swfDelta = timings.framesRun >= fpsSampleStartSwfFrames
                    ? (timings.framesRun - fpsSampleStartSwfFrames)
                    : 0
                currentRenderFPS = Double(fpsSampleRenderCount) / elapsed
                currentSwfFPS = Double(swfDelta) / elapsed
                fpsSampleStartTimestamp = now
                fpsSampleRenderCount = 0
                fpsSampleStartSwfFrames = timings.framesRun
            }
        }
    }

    // MARK: - Touch Handling

    private func pixelLocation(for touch: UITouch) -> CGPoint {
        syncDrawableSizeIfNeeded()
        let pt = touch.location(in: self)
        let scale = effectiveScaleFactor
        return CGPoint(x: pt.x * scale, y: pt.y * scale)
    }

    private func syncKeyboardWithFocusState() {
        guard let player else { return }
        if player.isTextFocused {
            if !isFirstResponder {
                _ = becomeFirstResponder()
            }
        } else if isFirstResponder {
            _ = resignFirstResponder()
        }
    }

    override public func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesBegan(touches, with: event)
        guard let touch = touches.first, let player else { return }
        let loc = pixelLocation(for: touch)
        player.touchDown(x: Double(loc.x), y: Double(loc.y))
        syncKeyboardWithFocusState()
        renderIfPossible()
    }

    override public func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesMoved(touches, with: event)
        guard let touch = touches.first, let player else { return }
        let loc = pixelLocation(for: touch)
        player.touchMove(x: Double(loc.x), y: Double(loc.y))
        if player.needsRender {
            renderIfPossible()
        }
    }

    override public func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesEnded(touches, with: event)
        guard let touch = touches.first, let player else { return }
        let loc = pixelLocation(for: touch)
        player.touchUp(x: Double(loc.x), y: Double(loc.y))
        syncKeyboardWithFocusState()
        renderIfPossible()
    }

    override public func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesCancelled(touches, with: event)
        guard let touch = touches.first, let player else { return }
        let loc = pixelLocation(for: touch)
        player.touchUp(x: Double(loc.x), y: Double(loc.y))
        renderIfPossible()
    }
}

/// Weak-referencing target wrapper to avoid `CADisplayLink` retain cycles.
@MainActor
private final class DisplayLinkTarget {
    private let handler: (CADisplayLink) -> Void

    init(handler: @escaping (CADisplayLink) -> Void) {
        self.handler = handler
    }

    @objc func step(_ link: CADisplayLink) {
        handler(link)
    }
}
#endif
