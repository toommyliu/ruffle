#if canImport(UIKit)
import UIKit

/// A dark-glass bar docked above the software keyboard that mirrors the focused Flash `TextField`,
/// so the user can see what they type even when the keyboard covers the stage.
@MainActor
final class KeyboardInputAccessoryView: UIView {
    var onClear: (() -> Void)?
    var onPaste: (() -> Void)?
    var onSend: (() -> Void)?
    var onDismiss: (() -> Void)?

    private let blurView = UIVisualEffectView(effect: UIBlurEffect(style: .systemChromeMaterialDark))
    private let pill = UIView()
    private let textLabel = UILabel()
    private let clearButton = UIButton(type: .system)
    private let eyeButton = UIButton(type: .system)
    private let pasteButton = UIButton(type: .system)
    private let dismissButton = UIButton(type: .system)
    private let sendButton = UIButton(type: .system)

    private var revealPassword = false
    private var lastState: RufflePlayer.FocusedTextState?

    override init(frame: CGRect) {
        super.init(frame: CGRect(x: 0, y: 0, width: frame.width, height: 52))
        autoresizingMask = .flexibleWidth
        setup()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    override var intrinsicContentSize: CGSize {
        CGSize(width: UIView.noIntrinsicMetric, height: 52)
    }

    private func icon(_ button: UIButton, _ name: String, tint: UIColor = .white) {
        var config = UIButton.Configuration.plain()
        config.image = UIImage(systemName: name)
        config.baseForegroundColor = tint
        config.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 8, bottom: 6, trailing: 8)
        button.configuration = config
        button.setContentHuggingPriority(.required, for: .horizontal)
        button.setContentCompressionResistancePriority(.required, for: .horizontal)
    }

    private func setup() {
        blurView.frame = bounds
        blurView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        addSubview(blurView)

        pill.backgroundColor = UIColor.white.withAlphaComponent(0.12)
        pill.layer.cornerRadius = 18
        pill.clipsToBounds = true

        textLabel.font = .monospacedSystemFont(ofSize: 16, weight: .regular)
        textLabel.textColor = .white
        textLabel.lineBreakMode = .byTruncatingHead
        textLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        icon(clearButton, "xmark.circle.fill", tint: UIColor.white.withAlphaComponent(0.6))
        icon(eyeButton, "eye")
        icon(pasteButton, "doc.on.clipboard")
        icon(dismissButton, "keyboard.chevron.compact.down")

        var send = UIButton.Configuration.filled()
        send.image = UIImage(systemName: "return")
        send.baseBackgroundColor = .systemBlue
        send.baseForegroundColor = .white
        send.cornerStyle = .capsule
        send.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 14, bottom: 6, trailing: 14)
        sendButton.configuration = send

        clearButton.addAction(UIAction { [weak self] _ in self?.onClear?() }, for: .touchUpInside)
        eyeButton.addAction(UIAction { [weak self] _ in
            guard let self else { return }
            self.revealPassword.toggle()
            self.update(state: self.lastState, force: true)
        }, for: .touchUpInside)
        pasteButton.addAction(UIAction { [weak self] _ in self?.onPaste?() }, for: .touchUpInside)
        dismissButton.addAction(UIAction { [weak self] _ in self?.onDismiss?() }, for: .touchUpInside)
        sendButton.addAction(UIAction { [weak self] _ in self?.onSend?() }, for: .touchUpInside)

        let pillStack = UIStackView(arrangedSubviews: [textLabel, clearButton, eyeButton])
        pillStack.axis = .horizontal
        pillStack.alignment = .center
        pillStack.spacing = 0
        pillStack.translatesAutoresizingMaskIntoConstraints = false
        pill.addSubview(pillStack)
        NSLayoutConstraint.activate([
            pillStack.leadingAnchor.constraint(equalTo: pill.leadingAnchor, constant: 12),
            pillStack.trailingAnchor.constraint(equalTo: pill.trailingAnchor, constant: -2),
            pillStack.topAnchor.constraint(equalTo: pill.topAnchor),
            pillStack.bottomAnchor.constraint(equalTo: pill.bottomAnchor),
        ])

        let row = UIStackView(arrangedSubviews: [pill, pasteButton, dismissButton, sendButton])
        row.axis = .horizontal
        row.alignment = .center
        row.spacing = 4
        row.translatesAutoresizingMaskIntoConstraints = false
        blurView.contentView.addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: blurView.contentView.safeAreaLayoutGuide.leadingAnchor, constant: 10),
            row.trailingAnchor.constraint(equalTo: blurView.contentView.safeAreaLayoutGuide.trailingAnchor, constant: -10),
            row.topAnchor.constraint(equalTo: blurView.contentView.topAnchor, constant: 8),
            row.bottomAnchor.constraint(equalTo: blurView.contentView.bottomAnchor, constant: -8),
            pill.heightAnchor.constraint(equalToConstant: 36),
        ])

        update(state: nil, force: true)
    }

    /// Refreshes the bar from the latest focused-field snapshot. No-op if nothing changed.
    func update(state: RufflePlayer.FocusedTextState?, force: Bool = false) {
        if !force && state == lastState { return }
        lastState = state

        guard let state else {
            textLabel.text = nil
            clearButton.isHidden = true
            eyeButton.isHidden = true
            return
        }

        let masked = state.isPassword && !revealPassword
        let shown = masked ? String(repeating: "\u{2022}", count: state.text.count) : state.text
        let utf16 = Array(shown.utf16)
        let caret = min(max(0, masked ? shown.count : state.caret), utf16.count)
        let withCaret = String(decoding: utf16[..<caret], as: UTF16.self)
            + "|"
            + String(decoding: utf16[caret...], as: UTF16.self)

        if state.text.isEmpty {
            textLabel.text = "Type here…"
            textLabel.textColor = UIColor.white.withAlphaComponent(0.4)
        } else {
            textLabel.text = withCaret
            textLabel.textColor = .white
        }
        clearButton.isHidden = state.text.isEmpty
        eyeButton.isHidden = !state.isPassword
        eyeButton.configuration?.image = UIImage(systemName: revealPassword ? "eye.slash" : "eye")
    }
}
#endif

