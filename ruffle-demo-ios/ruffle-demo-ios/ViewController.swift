//
//  ViewController.swift
//  ruffle-demo-ios
//
//  Created by Dika on 03/11/2026.
//

import Darwin
import FlashPlayerKit
import QuartzCore
import UIKit

struct TelemetryEntry {
    let timestamp: String
    let method: String
    let detail: String
    let isSystem: Bool
}

struct PerformanceSample {
    let cpuPercent: Double
    let ramMB: Double
    let swfFPS: Double
    let renderFPS: Double
    let tickMs: Double
    let renderMs: Double
}

/// Samples process-level CPU usage (%) and physical memory footprint (MB) via Darwin Mach kernel APIs.
@MainActor
final class SystemMetricsMonitor {
    private(set) var samples: [PerformanceSample] = []
    let maxSamples = 60

    func recordSample(
        swfFPS: Double,
        renderFPS: Double,
        tickMs: Double,
        renderMs: Double
    ) -> PerformanceSample {
        let sample = PerformanceSample(
            cpuPercent: Self.readProcessCPUPercent(),
            ramMB: Self.readPhysicalFootprintMB(),
            swfFPS: swfFPS,
            renderFPS: renderFPS,
            tickMs: tickMs,
            renderMs: renderMs
        )
        samples.append(sample)
        if samples.count > maxSamples {
            samples.removeFirst(samples.count - maxSamples)
        }
        return sample
    }

    static func readPhysicalFootprintMB() -> Double {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let kerr: kern_return_t = withUnsafeMutablePointer(to: &info) { infoPtr in
            infoPtr.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { intPtr in
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), intPtr, &count)
            }
        }
        guard kerr == KERN_SUCCESS else { return 0.0 }
        return Double(info.phys_footprint) / (1024.0 * 1024.0)
    }

    static func readProcessCPUPercent() -> Double {
        var threadsList: thread_act_array_t?
        var threadCount: mach_msg_type_number_t = 0
        let kerr = task_threads(mach_task_self_, &threadsList, &threadCount)
        guard kerr == KERN_SUCCESS, let threads = threadsList else { return 0.0 }
        defer {
            let size = vm_size_t(Int(threadCount) * MemoryLayout<thread_t>.stride)
            vm_deallocate(mach_task_self_, vm_address_t(bitPattern: threads), size)
        }

        var totalCPU: Double = 0.0
        for i in 0 ..< Int(threadCount) {
            var threadInfo = thread_basic_info()
            var threadInfoCount = mach_msg_type_number_t(THREAD_INFO_MAX)
            let kr: kern_return_t = withUnsafeMutablePointer(to: &threadInfo) { infoPtr in
                infoPtr.withMemoryRebound(to: integer_t.self, capacity: Int(threadInfoCount)) { intPtr in
                    thread_info(threads[i], thread_flavor_t(THREAD_BASIC_INFO), intPtr, &threadInfoCount)
                }
            }
            if kr == KERN_SUCCESS && (threadInfo.flags & TH_FLAGS_IDLE) == 0 {
                totalCPU += (Double(threadInfo.cpu_usage) / Double(TH_USAGE_SCALE)) * 100.0
            }
        }
        return totalCPU
    }
}

final class ViewController: UIViewController {
    private let flashPlayerView = UIFlashPlayerView(frame: .zero)
    private let metricsMonitor = SystemMetricsMonitor()
    private var metricsTimer: Timer?

    // Floating native iOS material pill button
    private let floatingBlurView = UIVisualEffectView(effect: UIBlurEffect(style: .systemChromeMaterialDark))
    private let floatingButton = UIButton(type: .system)
    private var floatingTopConstraint: NSLayoutConstraint?
    private var floatingTrailingConstraint: NSLayoutConstraint?

    private var entries: [TelemetryEntry] = []
    private var loadProgress: Int?
    private var isMovieLoaded = false
    private var isVerboseLoggingEnabled = false
    private var logsDirty = false
    private weak var activeInspector: InspectorSheetViewController?

    private static let timestampFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss.SSS"
        return formatter
    }()

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
        startMetricsTimer()

        // Re-take first responder after the keyboard goes away so shake events keep reaching us.
        NotificationCenter.default.addObserver(
            forName: UIResponder.keyboardDidHideNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { _ = self?.becomeFirstResponder() }
        }
    }

    // MARK: - Shake to show/hide the floating button

    override var canBecomeFirstResponder: Bool { true }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        _ = becomeFirstResponder()
    }

    override func motionEnded(_ motion: UIEvent.EventSubtype, with event: UIEvent?) {
        super.motionEnded(motion, with: event)
        guard motion == .motionShake else { return }
        let show = floatingBlurView.alpha < 0.5
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        floatingBlurView.isUserInteractionEnabled = show
        UIView.animate(withDuration: 0.2) {
            self.floatingBlurView.alpha = show ? 1 : 0
        }
    }

    deinit {
        metricsTimer?.invalidate()
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
        config.baseForegroundColor = .white
        config.cornerStyle = .capsule
        config.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 12, bottom: 6, trailing: 12)
        config.imagePadding = 6
        config.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { incoming in
            var outgoing = incoming
            outgoing.font = UIFont.monospacedDigitSystemFont(ofSize: 12, weight: .semibold)
            return outgoing
        }
        floatingButton.configuration = config
        floatingButton.translatesAutoresizingMaskIntoConstraints = false
        floatingButton.addTarget(self, action: #selector(presentLogsSheet), for: .touchUpInside)
        floatingBlurView.contentView.addSubview(floatingButton)

        let topConstraint = floatingBlurView.topAnchor.constraint(
            equalTo: view.safeAreaLayoutGuide.topAnchor,
            constant: 12
        )
        let trailingConstraint = floatingBlurView.trailingAnchor.constraint(
            equalTo: view.safeAreaLayoutGuide.trailingAnchor,
            constant: -16
        )
        self.floatingTopConstraint = topConstraint
        self.floatingTrailingConstraint = trailingConstraint

        NSLayoutConstraint.activate([
            topConstraint,
            trailingConstraint,
            floatingBlurView.heightAnchor.constraint(equalToConstant: 36),

            floatingButton.topAnchor.constraint(equalTo: floatingBlurView.contentView.topAnchor),
            floatingButton.bottomAnchor.constraint(equalTo: floatingBlurView.contentView.bottomAnchor),
            floatingButton.leadingAnchor.constraint(equalTo: floatingBlurView.contentView.leadingAnchor),
            floatingButton.trailingAnchor.constraint(equalTo: floatingBlurView.contentView.trailingAnchor),
        ])

        let pan = UIPanGestureRecognizer(target: self, action: #selector(handleFloatingPan(_:)))
        floatingBlurView.addGestureRecognizer(pan)

        updateFloatingBadge(latestSample: nil)
    }

    private func updateFloatingBadge(latestSample: PerformanceSample? = nil) {
        var config = floatingButton.configuration ?? .plain()
        if let pct = loadProgress, !isMovieLoaded {
            config.title = "Loading \(pct)%"
            config.image = UIImage(
                systemName: "arrow.down.circle.fill",
                withConfiguration: UIImage.SymbolConfiguration(pointSize: 12, weight: .semibold)
            )
            config.baseForegroundColor = .systemOrange
        } else if let sample = latestSample {
            let fpsInt = Int(sample.swfFPS.rounded())
            let cpuInt = Int(sample.cpuPercent.rounded())
            let ramInt = Int(sample.ramMB.rounded())
            config.title = "\(fpsInt) FPS • \(cpuInt)% • \(ramInt) MB"
            config.image = UIImage(
                systemName: "speedometer",
                withConfiguration: UIImage.SymbolConfiguration(pointSize: 12, weight: .semibold)
            )
            config.baseForegroundColor = sample.swfFPS >= 20 ? .white : .systemYellow
        } else {
            config.title = "Stats & Logs"
            config.image = UIImage(
                systemName: "speedometer",
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

    // MARK: - Telemetry Timer

    private func startMetricsTimer() {
        metricsTimer?.invalidate()
        metricsTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.sampleMetrics()
            }
        }
    }

    private func sampleMetrics() {
        let timings = flashPlayerView.player?.frameTimings
            ?? RuffleFrameTimings(tickMs: 0, renderMs: 0, framesRun: 0, swfFrameRate: 24)
        let sample = metricsMonitor.recordSample(
            swfFPS: flashPlayerView.currentSwfFPS,
            renderFPS: flashPlayerView.currentRenderFPS,
            tickMs: timings.tickMs,
            renderMs: timings.renderMs
        )
        updateFloatingBadge(latestSample: sample)

        if let inspector = activeInspector {
            inspector.updateMetrics(
                samples: metricsMonitor.samples,
                entries: logsDirty ? entries : nil
            )
            logsDirty = false
        }
    }

    // MARK: - Player Integration

    private func wirePlayerCallbacks() {
        flashPlayerView.player?.onExternalCall = { [weak self] name, args in
            guard let self else { return }

            if name == "onProgress" || name == "progress",
               let pct = args.first as? Int ?? (args.first as? Double).map(Int.init) {
                self.loadProgress = pct
                self.updateFloatingBadge(latestSample: self.metricsMonitor.samples.last)
                if let last = self.entries.last, last.method == name {
                    self.entries[self.entries.count - 1] = TelemetryEntry(
                        timestamp: Self.currentTimestamp(),
                        method: name,
                        detail: "\(pct)%",
                        isSystem: false
                    )
                    self.logsDirty = true
                    return
                }
            } else if name == "onLoaded" || name == "loaded" {
                self.isMovieLoaded = true
                self.updateFloatingBadge(latestSample: self.metricsMonitor.samples.last)
            } else if !self.isVerboseLoggingEnabled {
                // Skip high-frequency in-game packet formatting when verbose logging is off
                return
            }

            let formattedArgs = args.map { "\($0)" }.joined(separator: ", ")
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
        if isSystem || isVerboseLoggingEnabled {
            print("[FlashPlayerKit] [\(entry.timestamp)] \(method): \(detail)")
        }
        entries.append(entry)
        if entries.count > 100 {
            entries.removeFirst(entries.count - 100)
        }
        logsDirty = true
    }

    private static func currentTimestamp() -> String {
        timestampFormatter.string(from: Date())
    }

    // MARK: - Native iOS Sheet Presentation

    @objc private func presentLogsSheet() {
        if activeInspector != nil {
            dismiss(animated: true)
            return
        }

        let inspector = InspectorSheetViewController(
            samples: metricsMonitor.samples,
            entries: entries,
            movieSize: flashPlayerView.player?.movieSize ?? .zero,
            isPlaying: flashPlayerView.player?.isPlaying ?? true,
            volume: flashPlayerView.player?.volume ?? 1.0,
            quality: flashPlayerView.player?.quality ?? .high,
            renderScale: flashPlayerView.renderScale,
            isSmoothMotionEnabled: flashPlayerView.player?.isSmoothMotionEnabled ?? false,
            isVerboseLoggingEnabled: isVerboseLoggingEnabled,
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
                self.logsDirty = true
            },
            onVolumeChanged: { [weak self] newVolume in
                self?.flashPlayerView.player?.volume = newVolume
            },
            onQualityChanged: { [weak self] newQuality in
                guard let self else { return }
                self.flashPlayerView.player?.quality = newQuality
                self.appendEntry(method: "StageQuality", detail: "Set to \(newQuality.title)", isSystem: true)
            },
            onRenderScaleChanged: { [weak self] newScale in
                guard let self else { return }
                self.flashPlayerView.renderScale = newScale
                self.appendEntry(
                    method: "RenderScale",
                    detail: String(format: "%.2fx", newScale),
                    isSystem: true
                )
            },
            onSmoothMotionChanged: { [weak self] enabled in
                guard let self else { return }
                self.flashPlayerView.player?.isSmoothMotionEnabled = enabled
                self.appendEntry(
                    method: "SmoothMotion",
                    detail: enabled ? "Enabled (Sub-frame interpolation)" : "Disabled",
                    isSystem: true
                )
            },
            onVerboseLoggingChanged: { [weak self] enabled in
                guard let self else { return }
                self.isVerboseLoggingEnabled = enabled
                self.appendEntry(
                    method: "PacketLogging",
                    detail: enabled ? "Verbose logging ON" : "Verbose logging OFF",
                    isSystem: true
                )
            },
            onReleaseMemory: { [weak self] in
                guard let self else { return }
                let beforeMB = SystemMetricsMonitor.readPhysicalFootprintMB()
                self.flashPlayerView.player?.releaseMemory()
                let afterMB = SystemMetricsMonitor.readPhysicalFootprintMB()
                self.appendEntry(
                    method: "ReleaseMemory",
                    detail: String(format: "GC + cache sweep (%.1f MB → %.1f MB)", beforeMB, afterMB),
                    isSystem: true
                )
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

// MARK: - Sparkline Graph View

final class SparklineGraphView: UIView {
    private let titleLabel = UILabel()
    private let valueLabel = UILabel()
    private let subtitleLabel = UILabel()
    private let lineLayer = CAShapeLayer()
    private let fillLayer = CAShapeLayer()
    private var values: [Double] = []
    private var fixedMax: Double?
    private var accentColor: UIColor = .systemGreen

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = UIColor.secondarySystemGroupedBackground
        layer.cornerRadius = 12
        layer.cornerCurve = .continuous
        clipsToBounds = true

        fillLayer.strokeColor = nil
        layer.addSublayer(fillLayer)

        lineLayer.fillColor = nil
        lineLayer.lineWidth = 2.0
        lineLayer.lineCap = .round
        lineLayer.lineJoin = .round
        layer.addSublayer(lineLayer)

        titleLabel.font = .systemFont(ofSize: 11, weight: .semibold)
        titleLabel.textColor = .secondaryLabel
        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        addSubview(titleLabel)

        valueLabel.font = .monospacedDigitSystemFont(ofSize: 18, weight: .bold)
        valueLabel.textColor = .label
        valueLabel.translatesAutoresizingMaskIntoConstraints = false
        addSubview(valueLabel)

        subtitleLabel.font = .monospacedDigitSystemFont(ofSize: 10, weight: .medium)
        subtitleLabel.textColor = .secondaryLabel
        subtitleLabel.translatesAutoresizingMaskIntoConstraints = false
        addSubview(subtitleLabel)

        NSLayoutConstraint.activate([
            titleLabel.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            titleLabel.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            titleLabel.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),

            valueLabel.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 2),
            valueLabel.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            valueLabel.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),

            subtitleLabel.topAnchor.constraint(equalTo: valueLabel.bottomAnchor, constant: 1),
            subtitleLabel.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            subtitleLabel.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func configure(
        title: String,
        valueText: String,
        subtitleText: String,
        values: [Double],
        fixedMax: Double?,
        color: UIColor
    ) {
        titleLabel.text = title
        valueLabel.text = valueText
        subtitleLabel.text = subtitleText
        self.values = values
        self.fixedMax = fixedMax
        self.accentColor = color
        lineLayer.strokeColor = color.cgColor
        fillLayer.fillColor = color.withAlphaComponent(0.18).cgColor
        setNeedsLayout()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        drawSparkline()
    }

    private func drawSparkline() {
        let w = bounds.width
        let h = bounds.height
        let chartTop: CGFloat = 54
        let chartHeight = max(16, h - chartTop - 6)
        guard w > 10, values.count >= 2 else {
            lineLayer.path = nil
            fillLayer.path = nil
            return
        }

        let maxVal = max(fixedMax ?? (values.max() ?? 1.0), 1.0)
        let stepX = w / CGFloat(max(1, values.count - 1))

        let linePath = UIBezierPath()
        let fillPath = UIBezierPath()

        for (index, val) in values.enumerated() {
            let x = CGFloat(index) * stepX
            let normalized = CGFloat(min(max(val / maxVal, 0.0), 1.0))
            let y = chartTop + chartHeight * (1.0 - normalized)
            let pt = CGPoint(x: x, y: y)
            if index == 0 {
                linePath.move(to: pt)
                fillPath.move(to: CGPoint(x: x, y: h))
                fillPath.addLine(to: pt)
            } else {
                linePath.addLine(to: pt)
                fillPath.addLine(to: pt)
            }
        }

        fillPath.addLine(to: CGPoint(x: CGFloat(values.count - 1) * stepX, y: h))
        fillPath.close()

        lineLayer.path = linePath.cgPath
        fillLayer.path = fillPath.cgPath
    }
}

final class PerformanceDashboardCell: UITableViewCell {
    static let reuseID = "PerformanceDashboardCell"

    private let stackView = UIStackView()
    private let fpsCard = SparklineGraphView()
    private let cpuCard = SparklineGraphView()
    private let ramCard = SparklineGraphView()

    override init(style: UITableViewCell.CellStyle, reuseIdentifier: String?) {
        super.init(style: style, reuseIdentifier: reuseIdentifier)
        selectionStyle = .none
        backgroundColor = .clear
        contentView.backgroundColor = .clear

        stackView.axis = .horizontal
        stackView.distribution = .fillEqually
        stackView.spacing = 10
        stackView.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(stackView)

        stackView.addArrangedSubview(fpsCard)
        stackView.addArrangedSubview(cpuCard)
        stackView.addArrangedSubview(ramCard)

        NSLayoutConstraint.activate([
            stackView.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 4),
            stackView.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -4),
            stackView.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            stackView.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            stackView.heightAnchor.constraint(equalToConstant: 104),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func update(with samples: [PerformanceSample]) {
        let latest = samples.last ?? PerformanceSample(
            cpuPercent: 0,
            ramMB: SystemMetricsMonitor.readPhysicalFootprintMB(),
            swfFPS: 0,
            renderFPS: 0,
            tickMs: 0,
            renderMs: 0
        )

        let fpsValues = samples.map(\.swfFPS)
        let cpuValues = samples.map(\.cpuPercent)
        let ramValues = samples.map(\.ramMB)

        let peakCPU = cpuValues.max() ?? latest.cpuPercent
        let peakRAM = ramValues.max() ?? latest.ramMB

        fpsCard.configure(
            title: "SWF / METAL FPS",
            valueText: String(format: "%.1f FPS", latest.swfFPS),
            subtitleText: String(
                format: "tick %.1fms • gpu %.1fms",
                latest.tickMs,
                latest.renderMs
            ),
            values: fpsValues,
            fixedMax: 30.0,
            color: latest.swfFPS >= 20.0 ? .systemGreen : .systemOrange
        )

        cpuCard.configure(
            title: "CPU USAGE",
            valueText: String(format: "%.0f%%", latest.cpuPercent),
            subtitleText: String(format: "Peak %.0f%% (All threads)", peakCPU),
            values: cpuValues,
            fixedMax: max(100.0, peakCPU * 1.15),
            color: latest.cpuPercent > 85.0 ? .systemOrange : .systemCyan
        )

        ramCard.configure(
            title: "RAM FOOTPRINT",
            valueText: String(format: "%.0f MB", latest.ramMB),
            subtitleText: String(format: "Peak %.0f MB (Physical)", peakRAM),
            values: ramValues,
            fixedMax: max(256.0, peakRAM * 1.2),
            color: .systemPurple
        )
    }
}

// MARK: - Native iOS Sheet View Controller

final class InspectorSheetViewController: UIViewController, UITableViewDataSource, UITableViewDelegate {
    private let tableView = UITableView(frame: .zero, style: .insetGrouped)
    private var samples: [PerformanceSample]
    private var entries: [TelemetryEntry]
    private var movieSize: CGSize
    private var isPlaying: Bool
    private var volume: Float
    private var quality: RuffleStageQuality
    private var renderScale: CGFloat
    private var isSmoothMotionEnabled: Bool
    private var isVerboseLoggingEnabled: Bool

    private let onTogglePlayPause: () -> Bool
    private let onReload: () -> Void
    private let onClearLogs: () -> Void
    private let onVolumeChanged: (Float) -> Void
    private let onQualityChanged: (RuffleStageQuality) -> Void
    private let onRenderScaleChanged: (CGFloat) -> Void
    private let onSmoothMotionChanged: (Bool) -> Void
    private let onVerboseLoggingChanged: (Bool) -> Void
    private let onReleaseMemory: () -> Void

    private static let scalePresets: [CGFloat] = [0.5, 0.75, 1.0]

    init(
        samples: [PerformanceSample],
        entries: [TelemetryEntry],
        movieSize: CGSize,
        isPlaying: Bool,
        volume: Float,
        quality: RuffleStageQuality,
        renderScale: CGFloat,
        isSmoothMotionEnabled: Bool,
        isVerboseLoggingEnabled: Bool,
        onTogglePlayPause: @escaping () -> Bool,
        onReload: @escaping () -> Void,
        onClearLogs: @escaping () -> Void,
        onVolumeChanged: @escaping (Float) -> Void,
        onQualityChanged: @escaping (RuffleStageQuality) -> Void,
        onRenderScaleChanged: @escaping (CGFloat) -> Void,
        onSmoothMotionChanged: @escaping (Bool) -> Void,
        onVerboseLoggingChanged: @escaping (Bool) -> Void,
        onReleaseMemory: @escaping () -> Void
    ) {
        self.samples = samples
        self.entries = entries
        self.movieSize = movieSize
        self.isPlaying = isPlaying
        self.volume = volume
        self.quality = quality
        self.renderScale = renderScale
        self.isSmoothMotionEnabled = isSmoothMotionEnabled
        self.isVerboseLoggingEnabled = isVerboseLoggingEnabled
        self.onTogglePlayPause = onTogglePlayPause
        self.onReload = onReload
        self.onClearLogs = onClearLogs
        self.onVolumeChanged = onVolumeChanged
        self.onQualityChanged = onQualityChanged
        self.onRenderScaleChanged = onRenderScaleChanged
        self.onSmoothMotionChanged = onSmoothMotionChanged
        self.onVerboseLoggingChanged = onVerboseLoggingChanged
        self.onReleaseMemory = onReleaseMemory
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Performance & Inspector"
        view.backgroundColor = .systemGroupedBackground

        updateBarButtons()

        tableView.translatesAutoresizingMaskIntoConstraints = false
        tableView.dataSource = self
        tableView.delegate = self
        tableView.register(PerformanceDashboardCell.self, forCellReuseIdentifier: PerformanceDashboardCell.reuseID)
        tableView.register(UITableViewCell.self, forCellReuseIdentifier: "ControlCell")
        tableView.register(UITableViewCell.self, forCellReuseIdentifier: "LogCell")
        view.addSubview(tableView)

        NSLayoutConstraint.activate([
            tableView.topAnchor.constraint(equalTo: view.topAnchor),
            tableView.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            tableView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            tableView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
        ])
    }

    func updateMetrics(samples: [PerformanceSample], entries: [TelemetryEntry]?) {
        self.samples = samples
        if let dashboardCell = tableView.cellForRow(at: IndexPath(row: 0, section: 0)) as? PerformanceDashboardCell {
            dashboardCell.update(with: samples)
        }
        if let entries {
            self.entries = entries
            tableView.reloadSections(IndexSet(integer: 2), with: .none)
        }
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

    @objc private func qualitySegmentChanged(_ sender: UISegmentedControl) {
        let newQuality = RuffleStageQuality(rawValue: UInt32(sender.selectedSegmentIndex)) ?? .high
        quality = newQuality
        onQualityChanged(newQuality)
    }

    @objc private func scaleSegmentChanged(_ sender: UISegmentedControl) {
        let idx = max(0, min(Self.scalePresets.count - 1, sender.selectedSegmentIndex))
        let newScale = Self.scalePresets[idx]
        renderScale = newScale
        onRenderScaleChanged(newScale)
    }

    @objc private func smoothMotionSwitchChanged(_ sender: UISwitch) {
        isSmoothMotionEnabled = sender.isOn
        onSmoothMotionChanged(sender.isOn)
    }

    @objc private func verboseLoggingSwitchChanged(_ sender: UISwitch) {
        isVerboseLoggingEnabled = sender.isOn
        onVerboseLoggingChanged(sender.isOn)
    }

    // MARK: - UITableViewDataSource

    func numberOfSections(in tableView: UITableView) -> Int {
        3
    }

    func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        switch section {
        case 0:
            return "Live Telemetry (30s Window)"
        case 1:
            return "Performance & Engine Settings"
        default:
            return "ExternalInterface & Loader Logs (\(entries.count))"
        }
    }

    func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        switch section {
        case 0:
            return 1
        case 1:
            return 6
        default:
            return max(1, entries.count)
        }
    }

    func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        switch indexPath.section {
        case 0:
            let cell = tableView.dequeueReusableCell(
                withIdentifier: PerformanceDashboardCell.reuseID,
                for: indexPath
            ) as! PerformanceDashboardCell
            cell.update(with: samples)
            return cell

        case 1:
            let cell = tableView.dequeueReusableCell(withIdentifier: "ControlCell", for: indexPath)
            var content = UIListContentConfiguration.valueCell()
            cell.selectionStyle = .none
            cell.accessoryView = nil
            cell.accessoryType = .none

            switch indexPath.row {
            case 0:
                content.image = UIImage(systemName: "sparkles.tv.fill")
                content.imageProperties.tintColor = .systemGreen
                content.text = "Stage Quality"
                let seg = UISegmentedControl(items: RuffleStageQuality.allCases.map(\.title))
                seg.selectedSegmentIndex = Int(quality.rawValue)
                seg.addTarget(self, action: #selector(qualitySegmentChanged(_:)), for: .valueChanged)
                cell.accessoryView = seg

            case 1:
                content.image = UIImage(systemName: "viewfinder")
                content.imageProperties.tintColor = .systemBlue
                content.text = "Resolution Scale"
                let seg = UISegmentedControl(items: ["0.5×", "0.75×", "1.0×"])
                if renderScale <= 0.55 {
                    seg.selectedSegmentIndex = 0
                } else if renderScale <= 0.85 {
                    seg.selectedSegmentIndex = 1
                } else {
                    seg.selectedSegmentIndex = 2
                }
                seg.addTarget(self, action: #selector(scaleSegmentChanged(_:)), for: .valueChanged)
                cell.accessoryView = seg

            case 2:
                content.image = UIImage(systemName: "hare.fill")
                content.imageProperties.tintColor = .systemCyan
                content.text = "Smooth Motion (60+ FPS)"
                let toggle = UISwitch()
                toggle.isOn = isSmoothMotionEnabled
                toggle.addTarget(self, action: #selector(smoothMotionSwitchChanged(_:)), for: .valueChanged)
                cell.accessoryView = toggle

            case 3:
                content.image = UIImage(systemName: "memorychip.fill")
                content.imageProperties.tintColor = .systemPurple
                content.text = "Free Unused Memory & GC"
                content.secondaryText = "Tap to sweep"
                cell.selectionStyle = .default
                cell.accessoryType = .disclosureIndicator

            case 4:
                content.image = UIImage(systemName: "doc.text.magnifyingglass")
                content.imageProperties.tintColor = .systemTeal
                content.text = "Verbose Packet Logging"
                let toggle = UISwitch()
                toggle.isOn = isVerboseLoggingEnabled
                toggle.addTarget(self, action: #selector(verboseLoggingSwitchChanged(_:)), for: .valueChanged)
                cell.accessoryView = toggle

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

        default:
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

    func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        if indexPath.section == 1 && indexPath.row == 3 {
            onReleaseMemory()
        }
    }
}
