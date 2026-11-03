//
//  ViewController.swift
//  ruffle-demo-ios
//
//  Created by farhandika on 03/11/26.
//

import FlashPlayerKit
import UIKit

struct TelemetryEntry {
    let timestamp: String
    let method: String
    let detail: String
    let isSystem: Bool
}

final class ViewController: UIViewController {
    private let flashPlayerView = UIFlashPlayerView(frame: .zero)

    // Floating native iOS material pill button
    private let floatingBlurView = UIVisualEffectView(effect: UIBlurEffect(style: .systemChromeMaterialDark))
    private let floatingButton = UIButton(type: .system)
    private var floatingTopConstraint: NSLayoutConstraint?
    private var floatingTrailingConstraint: NSLayoutConstraint?

    private var entries: [TelemetryEntry] = []
    private var loadProgress: Int?
    private var isMovieLoaded = false
    private weak var activeInspector: InspectorSheetViewController?

    override var prefersStatusBarHidden: Bool {
        true
    }

    override var prefersHomeIndicatorAutoHidden: Bool {
        true
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        setupFullScreenPlayer()
        setupFloatingButton()
        wirePlayerCallbacks()
        loadBundledSwf()
    }

    // MARK: - UI Setup

    private func setupFullScreenPlayer() {
        flashPlayerView.translatesAutoresizingMaskIntoConstraints = false
        flashPlayerView.backgroundColor = .black
        view.addSubview(flashPlayerView)

        NSLayoutConstraint.activate([
            flashPlayerView.topAnchor.constraint(equalTo: view.topAnchor),
            flashPlayerView.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            flashPlayerView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            flashPlayerView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
        ])
    }

    private func setupFloatingButton() {
        floatingBlurView.translatesAutoresizingMaskIntoConstraints = false
        floatingBlurView.layer.cornerRadius = 18
        floatingBlurView.layer.cornerCurve = .continuous
        floatingBlurView.clipsToBounds = true
        floatingBlurView.layer.borderWidth = 0.5
        floatingBlurView.layer.borderColor = UIColor.white.withAlphaComponent(0.22).cgColor
        view.addSubview(floatingBlurView)

        var config = UIButton.Configuration.plain()
        config.image = UIImage(
            systemName: "terminal.fill",
            withConfiguration: UIImage.SymbolConfiguration(pointSize: 12, weight: .semibold)
        )
        config.title = "Logs"
        config.imagePadding = 6
        config.contentInsets = NSDirectionalEdgeInsets(top: 7, leading: 12, bottom: 7, trailing: 12)
        config.baseForegroundColor = .white
        config.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { incoming in
            var outgoing = incoming
            outgoing.font = UIFont.systemFont(ofSize: 13, weight: .semibold)
            return outgoing
        }

        floatingButton.translatesAutoresizingMaskIntoConstraints = false
        floatingButton.configuration = config
        floatingButton.addTarget(self, action: #selector(presentLogsSheet), for: .touchUpInside)
        floatingBlurView.contentView.addSubview(floatingButton)

        let panGesture = UIPanGestureRecognizer(target: self, action: #selector(handleFloatingPan(_:)))
        floatingBlurView.addGestureRecognizer(panGesture)

        let safe = view.safeAreaLayoutGuide
        let topConstraint = floatingBlurView.topAnchor.constraint(equalTo: safe.topAnchor, constant: 12)
        let trailingConstraint = floatingBlurView.trailingAnchor.constraint(equalTo: safe.trailingAnchor, constant: -16)
        self.floatingTopConstraint = topConstraint
        self.floatingTrailingConstraint = trailingConstraint

        NSLayoutConstraint.activate([
            topConstraint,
            trailingConstraint,
            floatingButton.topAnchor.constraint(equalTo: floatingBlurView.contentView.topAnchor),
            floatingButton.bottomAnchor.constraint(equalTo: floatingBlurView.contentView.bottomAnchor),
            floatingButton.leadingAnchor.constraint(equalTo: floatingBlurView.contentView.leadingAnchor),
            floatingButton.trailingAnchor.constraint(equalTo: floatingBlurView.contentView.trailingAnchor),
        ])
    }

    private func updateFloatingBadge() {
        guard var config = floatingButton.configuration else { return }
        if let pct = loadProgress, !isMovieLoaded {
            config.title = "\(pct)%"
            config.image = UIImage(
                systemName: "arrow.down.circle.fill",
                withConfiguration: UIImage.SymbolConfiguration(pointSize: 12, weight: .semibold)
            )
            config.baseForegroundColor = .systemOrange
        } else {
            config.title = "Logs"
            config.image = UIImage(
                systemName: "terminal.fill",
                withConfiguration: UIImage.SymbolConfiguration(pointSize: 12, weight: .semibold)
            )
            config.baseForegroundColor = .white
        }
        floatingButton.configuration = config
    }

    @objc private func handleFloatingPan(_ gesture: UIPanGestureRecognizer) {
        guard let topConstraint = floatingTopConstraint,
              let trailingConstraint = floatingTrailingConstraint else { return }

        let translation = gesture.translation(in: view)
        gesture.setTranslation(.zero, in: view)

        topConstraint.constant += translation.y
        trailingConstraint.constant += translation.x

        if gesture.state == .ended || gesture.state == .cancelled {
            let safeFrame = view.safeAreaLayoutGuide.layoutFrame
            let buttonWidth = floatingBlurView.bounds.width
            let buttonHeight = floatingBlurView.bounds.height

            let minTop: CGFloat = 12
            let maxTop = max(minTop, safeFrame.height - buttonHeight - 12)
            let clampedTop = min(max(topConstraint.constant, minTop), maxTop)

            // Snap to either the leading or trailing safe-area edge
            let currentCenterX = floatingBlurView.frame.midX
            let snapToLeading = currentCenterX < view.bounds.midX
            let targetTrailing: CGFloat = snapToLeading
                ? -(safeFrame.width - buttonWidth - 16)
                : -16

            topConstraint.constant = clampedTop
            trailingConstraint.constant = targetTrailing

            UIView.animate(
                withDuration: 0.35,
                delay: 0,
                usingSpringWithDamping: 0.78,
                initialSpringVelocity: 0.5,
                options: [.allowUserInteraction, .beginFromCurrentState]
            ) {
                self.view.layoutIfNeeded()
            }
        }
    }

    // MARK: - Player Integration

    private func wirePlayerCallbacks() {
        flashPlayerView.player?.onExternalCall = { [weak self] name, args in
            guard let self else { return }
            let formattedArgs = args.map { "\($0)" }.joined(separator: ", ")

            if name == "onProgress" || name == "progress", let pct = args.first as? Int ?? (args.first as? Double).map(Int.init) {
                self.loadProgress = pct
                self.updateFloatingBadge()
                // Coalesce consecutive progress entries so the log stays clean
                if let last = self.entries.last, last.method == name {
                    self.entries[self.entries.count - 1] = TelemetryEntry(
                        timestamp: Self.currentTimestamp(),
                        method: name,
                        detail: "\(pct)%",
                        isSystem: false
                    )
                    self.activeInspector?.reloadData(
                        entries: self.entries,
                        movieSize: self.flashPlayerView.player?.movieSize ?? .zero,
                        isPlaying: self.flashPlayerView.player?.isPlaying ?? true
                    )
                    return
                }
            } else if name == "onLoaded" || name == "loaded" {
                self.isMovieLoaded = true
                self.updateFloatingBadge()
            }

            self.appendEntry(
                method: name,
                detail: formattedArgs.isEmpty ? "—" : formattedArgs,
                isSystem: false
            )
        }
    }

    private func loadBundledSwf() {
        isMovieLoaded = false
        loadProgress = nil
        updateFloatingBadge()

        guard let swfURL = Bundle.main.url(forResource: "loader", withExtension: "swf") else {
            appendEntry(method: "Error", detail: "loader.swf not found in Bundle.main", isSystem: true)
            return
        }

        do {
            try flashPlayerView.load(fileURL: swfURL)
            if let size = flashPlayerView.player?.movieSize {
                appendEntry(
                    method: "Loaded Root SWF",
                    detail: "\(swfURL.lastPathComponent) (\(Int(size.width))×\(Int(size.height)) pt)",
                    isSystem: true
                )
            }
        } catch {
            appendEntry(method: "Load Error", detail: error.localizedDescription, isSystem: true)
        }
    }

    private func appendEntry(method: String, detail: String, isSystem: Bool) {
        let entry = TelemetryEntry(
            timestamp: Self.currentTimestamp(),
            method: method,
            detail: detail,
            isSystem: isSystem
        )
        print("[FlashPlayerKit] [\(entry.timestamp)] \(method): \(detail)")
        entries.append(entry)
        if entries.count > 100 {
            entries.removeFirst(entries.count - 100)
        }
        activeInspector?.reloadData(
            entries: entries,
            movieSize: flashPlayerView.player?.movieSize ?? .zero,
            isPlaying: flashPlayerView.player?.isPlaying ?? true
        )
    }

    private static func currentTimestamp() -> String {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss.SSS"
        return formatter.string(from: Date())
    }

    // MARK: - Native iOS Sheet Presentation

    @objc private func presentLogsSheet() {
        if activeInspector != nil {
            dismiss(animated: true)
            return
        }

        let inspector = InspectorSheetViewController(
            entries: entries,
            movieSize: flashPlayerView.player?.movieSize ?? .zero,
            isPlaying: flashPlayerView.player?.isPlaying ?? true,
            volume: flashPlayerView.player?.volume ?? 1.0,
            onTogglePlayPause: { [weak self] in
                guard let self, let player = self.flashPlayerView.player else { return true }
                player.isPlaying.toggle()
                let state = player.isPlaying
                self.appendEntry(method: "Playback", detail: state ? "Resumed" : "Paused", isSystem: true)
                return state
            },
            onReload: { [weak self] in
                guard let self else { return }
                self.appendEntry(method: "Reload", detail: "Reloading loader.swf", isSystem: true)
                self.loadBundledSwf()
            },
            onClearLogs: { [weak self] in
                guard let self else { return }
                self.entries.removeAll()
                self.activeInspector?.reloadData(
                    entries: self.entries,
                    movieSize: self.flashPlayerView.player?.movieSize ?? .zero,
                    isPlaying: self.flashPlayerView.player?.isPlaying ?? true
                )
            },
            onVolumeChanged: { [weak self] newVolume in
                self?.flashPlayerView.player?.volume = newVolume
            }
        )
        self.activeInspector = inspector

        let nav = UINavigationController(rootViewController: inspector)
        nav.overrideUserInterfaceStyle = .dark
        if let sheet = nav.sheetPresentationController {
            sheet.detents = [.medium(), .large()]
            sheet.prefersGrabberVisible = true
            sheet.prefersScrollingExpandsWhenScrolledToEdge = false
            sheet.largestUndimmedDetentIdentifier = .medium
            sheet.preferredCornerRadius = 20
        }
        present(nav, animated: true)
    }
}

// MARK: - Native iOS Sheet View Controller

final class InspectorSheetViewController: UIViewController, UITableViewDataSource, UITableViewDelegate {
    private let tableView = UITableView(frame: .zero, style: .insetGrouped)
    private var entries: [TelemetryEntry]
    private var movieSize: CGSize
    private var isPlaying: Bool
    private var volume: Float

    private let onTogglePlayPause: () -> Bool
    private let onReload: () -> Void
    private let onClearLogs: () -> Void
    private let onVolumeChanged: (Float) -> Void

    init(
        entries: [TelemetryEntry],
        movieSize: CGSize,
        isPlaying: Bool,
        volume: Float,
        onTogglePlayPause: @escaping () -> Bool,
        onReload: @escaping () -> Void,
        onClearLogs: @escaping () -> Void,
        onVolumeChanged: @escaping (Float) -> Void
    ) {
        self.entries = entries
        self.movieSize = movieSize
        self.isPlaying = isPlaying
        self.volume = volume
        self.onTogglePlayPause = onTogglePlayPause
        self.onReload = onReload
        self.onClearLogs = onClearLogs
        self.onVolumeChanged = onVolumeChanged
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "FlashPlayerKit Inspector"
        view.backgroundColor = .systemGroupedBackground

        updateBarButtons()

        tableView.translatesAutoresizingMaskIntoConstraints = false
        tableView.dataSource = self
        tableView.delegate = self
        tableView.register(UITableViewCell.self, forCellReuseIdentifier: "StatusCell")
        tableView.register(UITableViewCell.self, forCellReuseIdentifier: "LogCell")
        view.addSubview(tableView)

        NSLayoutConstraint.activate([
            tableView.topAnchor.constraint(equalTo: view.topAnchor),
            tableView.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            tableView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            tableView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
        ])
    }

    func reloadData(entries: [TelemetryEntry], movieSize: CGSize, isPlaying: Bool) {
        self.entries = entries
        self.movieSize = movieSize
        self.isPlaying = isPlaying
        updateBarButtons()
        tableView.reloadData()
    }

    private func updateBarButtons() {
        let playPauseItem = UIBarButtonItem(
            image: UIImage(systemName: isPlaying ? "pause.fill" : "play.fill"),
            style: .plain,
            target: self,
            action: #selector(didTapPlayPause)
        )
        let reloadItem = UIBarButtonItem(
            image: UIImage(systemName: "arrow.clockwise"),
            style: .plain,
            target: self,
            action: #selector(didTapReload)
        )
        navigationItem.leftBarButtonItems = [playPauseItem, reloadItem]

        let clearItem = UIBarButtonItem(
            image: UIImage(systemName: "trash"),
            style: .plain,
            target: self,
            action: #selector(didTapClear)
        )
        let doneItem = UIBarButtonItem(
            barButtonSystemItem: .done,
            target: self,
            action: #selector(didTapDone)
        )
        navigationItem.rightBarButtonItems = [doneItem, clearItem]
    }

    @objc private func didTapPlayPause() {
        isPlaying = onTogglePlayPause()
        updateBarButtons()
    }

    @objc private func didTapReload() {
        onReload()
    }

    @objc private func didTapClear() {
        onClearLogs()
    }

    @objc private func didTapDone() {
        dismiss(animated: true)
    }

    @objc private func volumeSliderChanged(_ sender: UISlider) {
        volume = sender.value
        onVolumeChanged(sender.value)
    }

    // MARK: - UITableViewDataSource

    func numberOfSections(in tableView: UITableView) -> Int {
        2
    }

    func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        section == 0 ? "Runtime" : "ExternalInterface & Loader Logs (\(entries.count))"
    }

    func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        if section == 0 {
            return 3
        }
        return max(1, entries.count)
    }

    func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        if indexPath.section == 0 {
            let cell = tableView.dequeueReusableCell(withIdentifier: "StatusCell", for: indexPath)
            var content = UIListContentConfiguration.valueCell()
            cell.selectionStyle = .none
            cell.accessoryView = nil

            switch indexPath.row {
            case 0:
                content.image = UIImage(systemName: "cpu.fill")
                content.imageProperties.tintColor = .systemGreen
                content.text = "Renderer"
                content.secondaryText = "Metal (CAMetalLayer)"
            case 1:
                content.image = UIImage(systemName: "aspectratio.fill")
                content.imageProperties.tintColor = .systemBlue
                content.text = "Stage Dimensions"
                content.secondaryText = "\(Int(movieSize.width)) × \(Int(movieSize.height)) pt"
            default:
                content.image = UIImage(systemName: "speaker.wave.2.fill")
                content.imageProperties.tintColor = .systemOrange
                content.text = "Audio Volume"
                let slider = UISlider(frame: CGRect(x: 0, y: 0, width: 140, height: 28))
                slider.minimumValue = 0.0
                slider.maximumValue = 1.0
                slider.value = volume
                slider.addTarget(self, action: #selector(volumeSliderChanged(_:)), for: .valueChanged)
                cell.accessoryView = slider
            }
            cell.contentConfiguration = content
            return cell
        } else {
            let cell = tableView.dequeueReusableCell(withIdentifier: "LogCell", for: indexPath)
            cell.selectionStyle = .none
            var content = UIListContentConfiguration.subtitleCell()

            if entries.isEmpty {
                content.text = "No events recorded"
                content.textProperties.color = .secondaryLabel
                content.secondaryText = nil
                cell.contentConfiguration = content
                return cell
            }

            // Show newest logs at top
            let entry = entries[entries.count - 1 - indexPath.row]
            content.image = UIImage(
                systemName: entry.isSystem ? "gearshape.fill" : "arrow.left.arrow.right.circle.fill"
            )
            content.imageProperties.tintColor = entry.isSystem ? .systemGray : .systemTeal
            content.text = "\(entry.method)  •  \(entry.detail)"
            content.textProperties.font = .monospacedSystemFont(ofSize: 13, weight: .semibold)
            content.secondaryText = entry.timestamp
            content.secondaryTextProperties.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
            content.secondaryTextProperties.color = .secondaryLabel
            cell.contentConfiguration = content
            return cell
        }
    }
}
