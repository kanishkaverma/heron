import UIKit

/// ChatGPT-style recording capsule: cancel · live waveform · stop · ↑.
/// The arrow means "transcribe into the draft", never "send to the agent".
final class InlineDictationBar: UIView {
    var onCancel: (() -> Void)?
    var onStop: (() -> Void)?
    var onTranscribe: (() -> Void)?
    private let cancel = UIButton(type: .system)
    private let stop = UIButton(type: .system)
    private let confirm = UIButton(type: .system)
    private let waveform = DictationWaveform()
    private let status = UILabel()
    private let spinner = UIActivityIndicatorView(style: .medium)
    private let blue = UIColor(red: 0.29, green: 0.50, blue: 0.94, alpha: 1)
    private var lastElapsed: TimeInterval = -1

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = UIColor { $0.userInterfaceStyle == .dark ? UIColor(white: 0.105, alpha: 1) : UIColor(white: 0.94, alpha: 1) }
        layer.borderWidth = 0.5
        accessibilityIdentifier = "dictation-inline-bar"
        for button in [cancel, stop, confirm] {
            button.translatesAutoresizingMaskIntoConstraints = false
            button.layer.cornerRadius = 22
            button.layer.borderWidth = 0.75
            button.tintColor = Palette.text
            button.setImage(UIImage(systemName: "xmark", withConfiguration: UIImage.SymbolConfiguration(pointSize: 18, weight: .regular)), for: .normal)
            addSubview(button)
            NSLayoutConstraint.activate([
                button.widthAnchor.constraint(equalToConstant: 44),
                button.heightAnchor.constraint(equalToConstant: 44),
                button.centerYAnchor.constraint(equalTo: centerYAnchor),
            ])
        }
        cancel.accessibilityLabel = "Cancel dictation"
        cancel.accessibilityIdentifier = "dictation-cancel"
        cancel.addAction(UIAction { [weak self] _ in self?.onCancel?() }, for: .touchUpInside)
        stop.accessibilityIdentifier = "dictation-stop"
        stop.addAction(UIAction { [weak self] _ in self?.onStop?() }, for: .touchUpInside)
        confirm.accessibilityLabel = "Transcribe recording"
        confirm.accessibilityHint = "Adds text to your draft without sending a message."
        confirm.accessibilityIdentifier = "dictation-transcribe"
        confirm.tintColor = .black
        confirm.backgroundColor = blue
        confirm.layer.borderWidth = 0
        confirm.setImage(UIImage(systemName: "arrow.up", withConfiguration: UIImage.SymbolConfiguration(pointSize: 23, weight: .regular)), for: .normal)
        confirm.addAction(UIAction { [weak self] _ in self?.onTranscribe?() }, for: .touchUpInside)
        waveform.translatesAutoresizingMaskIntoConstraints = false
        waveform.isAccessibilityElement = true
        waveform.accessibilityIdentifier = "dictation-waveform"
        addSubview(waveform)
        status.translatesAutoresizingMaskIntoConstraints = false
        status.textAlignment = .center
        status.font = .preferredFont(forTextStyle: .caption1)
        status.adjustsFontForContentSizeCategory = true
        status.textColor = Palette.secondary
        status.numberOfLines = 2
        status.accessibilityIdentifier = "dictation-status"
        addSubview(status)
        spinner.color = .black
        spinner.translatesAutoresizingMaskIntoConstraints = false
        confirm.addSubview(spinner)
        NSLayoutConstraint.activate([
            cancel.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 9),
            confirm.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -9),
            stop.trailingAnchor.constraint(equalTo: confirm.leadingAnchor, constant: -9),
            waveform.leadingAnchor.constraint(equalTo: cancel.trailingAnchor, constant: 12),
            waveform.trailingAnchor.constraint(equalTo: stop.leadingAnchor, constant: -12),
            waveform.centerYAnchor.constraint(equalTo: centerYAnchor),
            waveform.heightAnchor.constraint(equalToConstant: 30),
            status.leadingAnchor.constraint(equalTo: waveform.leadingAnchor),
            status.trailingAnchor.constraint(equalTo: waveform.trailingAnchor),
            status.centerYAnchor.constraint(equalTo: centerYAnchor),
            spinner.centerXAnchor.constraint(equalTo: confirm.centerXAnchor),
            spinner.centerYAnchor.constraint(equalTo: confirm.centerYAnchor),
        ])
        refreshBorders()
    }

    required init?(coder: NSCoder) { fatalError() }

    func reset() {
        lastElapsed = -1
        waveform.reset()
    }

    func render(_ session: DictationSession) {
        let busy = session.phase == .transcribing || session.phase == .requesting
        let recording = session.phase == .recording
        let paused = session.phase == .paused
        stop.isEnabled = (recording || paused) && session.elapsed < 300
        stop.alpha = stop.isEnabled ? 1 : 0.35
        stop.accessibilityLabel = paused ? "Resume dictation" : "Stop recording"
        stop.setImage(UIImage(systemName: paused ? "play.fill" : "stop.fill", withConfiguration: UIImage.SymbolConfiguration(pointSize: paused ? 14 : 13, weight: .regular)), for: .normal)
        confirm.isEnabled = !busy
        confirm.accessibilityLabel = session.phase == .failed ? "Retry transcription" : "Transcribe recording"
        confirm.imageView?.alpha = busy ? 0 : 1
        if busy { spinner.startAnimating() } else { spinner.stopAnimating() }
        status.isHidden = recording || paused
        switch session.phase {
        case .requesting: status.text = "Starting mic…"
        case .transcribing: status.text = "Transcribing…"
        case .failed: status.text = "Couldn't transcribe\nTap ↑ to retry"
        case .recording, .paused: status.text = nil
        }
        waveform.isHidden = !status.isHidden
        if recording, session.elapsed != lastElapsed {
            waveform.append(session.level)
            lastElapsed = session.elapsed
        }
        waveform.accessibilityLabel = recording ? "Recording" : "Recording paused"
        waveform.accessibilityValue = String(format: "%d:%02d", Int(session.elapsed) / 60, Int(session.elapsed) % 60)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        layer.cornerRadius = bounds.height / 2
        refreshBorders()
    }

    private func refreshBorders() {
        let border = UIColor { $0.userInterfaceStyle == .dark ? UIColor(white: 0.28, alpha: 1) : UIColor(white: 0.76, alpha: 1) }
        cancel.layer.borderColor = border.resolvedColor(with: traitCollection).cgColor
        stop.layer.borderColor = border.resolvedColor(with: traitCollection).cgColor
        layer.borderColor = border.withAlphaComponent(0.5).resolvedColor(with: traitCollection).cgColor
    }
}

/// Actual microphone levels form a scrolling strip; silence is a dotted line.
private final class DictationWaveform: UIView {
    private var samples = [CGFloat](repeating: 0, count: 100)
    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .clear
        isOpaque = false
    }
    required init?(coder: NSCoder) { fatalError() }
    func reset() { samples = [CGFloat](repeating: 0, count: 100); setNeedsDisplay() }
    func append(_ level: CGFloat) {
        samples.removeFirst()
        samples.append(max(0, min(1, level)))
        setNeedsDisplay()
    }
    override func draw(_ rect: CGRect) {
        let count = max(1, Int(bounds.width / 5))
        let visible = samples.suffix(min(count, samples.count))
        let spacing = bounds.width / CGFloat(max(1, visible.count))
        for (index, amplitude) in visible.enumerated() {
            let height = max(3, min(28, amplitude * 28))
            let alpha = 0.25 + 0.60 * CGFloat(index + 1) / CGFloat(visible.count)
            Palette.text.withAlphaComponent(alpha).setFill()
            let x = spacing * (CGFloat(index) + 0.5) - 1.5
            UIBezierPath(roundedRect: CGRect(x: x, y: (bounds.height - height) / 2, width: 3, height: height), cornerRadius: 1.5).fill()
        }
    }
}
