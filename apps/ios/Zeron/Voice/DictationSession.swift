import AVFoundation
import UIKit

/// Owns one inline dictation. The only upload trigger is the recording bar's ↑.
/// No chat sending, presentation, or composer state belongs in this object.
final class DictationSession: NSObject, AVAudioRecorderDelegate {
    enum Phase: Equatable { case requesting, recording, paused, transcribing, failed }
    private(set) var phase: Phase = .requesting
    private(set) var elapsed: TimeInterval = 0
    private(set) var level: CGFloat = 0
    private(set) var message: String?
    var onUpdate: (() -> Void)?
    var onTranscript: ((String) -> Void)?
    var onFailure: ((String) -> Void)?

    private var recorder: AVAudioRecorder?
    private var recordingURL: URL?
    private var task: Task<Void, Never>?
    private var meter: Timer?
    private var ownsAudioSession = false
    private var cancelled = false
    private var observers: [NSObjectProtocol] = []
    private var fixture = false

    func start() {
        for name in [UIApplication.didEnterBackgroundNotification, AVAudioSession.interruptionNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.pause()
            })
        }
        #if targetEnvironment(simulator)
        // An audio-input seam only: credentials, Groq and composer insertion stay
        // real. This path is never compiled into physical-device builds.
        if let path = ProcessInfo.processInfo.environment["ZERON_DICTATION_TEST_AUDIO"] {
            do {
                let ext = URL(fileURLWithPath: path).pathExtension == "wav" ? "wav" : "m4a"
                let url = temporaryURL(ext)
                try FileManager.default.copyItem(at: URL(fileURLWithPath: path), to: url)
                recordingURL = url
                fixture = true
                phase = .recording
                startMeter()
                onUpdate?()
            } catch { failToStart("The test audio could not be loaded.") }
            return
        }
        #endif
        task = Task { @MainActor [weak self] in
            let allowed = await AVAudioApplication.requestRecordPermission()
            guard !Task.isCancelled, let self, !self.cancelled else { return }
            guard allowed else {
                self.failToStart("Microphone access is off. Turn it on in iOS Settings → Heron → Microphone.")
                return
            }
            do { try self.beginRecording() }
            catch { self.failToStart(error.localizedDescription) }
        }
    }

    private func beginRecording() throws {
        let session = AVAudioSession.sharedInstance()
        guard session.mode != .voiceChat && session.mode != .videoChat else {
            throw RecordingError("End your voice call before starting dictation.")
        }
        let url = temporaryURL("wav")
        recordingURL = url
        try session.setCategory(.record, mode: .default)
        ownsAudioSession = true
        try session.setActive(true)
        let recorder = try AVAudioRecorder(url: url, settings: [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: 16000,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
            AVLinearPCMIsBigEndianKey: false,
        ])
        self.recorder = recorder
        recorder.delegate = self
        recorder.isMeteringEnabled = true
        guard recorder.record(forDuration: 300) else { throw RecordingError("Could not start the microphone. Please retry.") }
        phase = .recording
        startMeter()
        onUpdate?()
    }

    private func startMeter() {
        meter?.invalidate()
        meter = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] _ in
            guard let self, self.phase == .recording else { return }
            if self.fixture {
                self.elapsed += 0.05
                self.level = CGFloat(0.14 + 0.36 * abs(sin(self.elapsed * 7)))
            } else if let recorder = self.recorder {
                recorder.updateMeters()
                self.elapsed = recorder.currentTime
                let amplitude = CGFloat(pow(10, recorder.averagePower(forChannel: 0) / 20))
                self.level = min(1, amplitude * 5)
            }
            if self.elapsed >= 300 { self.pause() }
            else { self.onUpdate?() }
        }
    }

    /// Stop freezes the waveform without uploading; play resumes the same file.
    func togglePause() {
        if phase == .recording { pause(); return }
        guard phase == .paused else { return }
        do {
            if !fixture {
                try AVAudioSession.sharedInstance().setActive(true)
                ownsAudioSession = true
                guard recorder?.record(forDuration: max(0.1, 300 - elapsed)) == true else {
                    throw RecordingError("Could not resume recording. Tap ↑ to transcribe what you have.")
                }
            }
            message = nil
            phase = .recording
            startMeter()
            onUpdate?()
        } catch { message = error.localizedDescription; onUpdate?() }
    }

    func pause() {
        guard phase == .recording else { return }
        recorder?.pause()
        meter?.invalidate()
        meter = nil
        deactivateAudio()
        phase = .paused
        level = 0
        onUpdate?()
    }

    func transcribe() {
        guard !cancelled, phase == .recording || phase == .paused || phase == .failed,
              let url = recordingURL else { return }
        stopCapture()
        phase = .transcribing
        message = nil
        onUpdate?()
        let pipeline = DictationPipeline(key: DictationSession.groqKey, vocabulary: DictationVocabulary.terms)
        task = Task { @MainActor [weak self] in
            do {
                let result = try await pipeline.transcribe(fileURL: url)
                DictationPipeline.log("transcribed via \(result.source.rawValue)")
                guard !Task.isCancelled, let self, !self.cancelled else { return }
                self.cleanupFile()
                self.onTranscript?(result.text)
            } catch {
                guard !Task.isCancelled, let self, !self.cancelled else { return }
                self.phase = .failed
                self.message = error.localizedDescription
                self.onUpdate?()
            }
        }
    }

    /// The Keychain key; in the simulator a test run may supply one through
    /// the environment instead (never compiled into device builds).
    static var groqKey: String {
        #if targetEnvironment(simulator)
        if let key = ProcessInfo.processInfo.environment["ZERON_DICTATION_TEST_GROQ_KEY"], !key.isEmpty { return key }
        #endif
        return GroqAPIKey.value
    }

    func cancel() {
        cancelled = true
        task?.cancel()
        stopCapture()
        cleanupFile()
        observers.forEach { NotificationCenter.default.removeObserver($0) }
        observers.removeAll()
    }

    private func failToStart(_ message: String) {
        cancel()
        onFailure?(message)
    }

    private func stopCapture() {
        meter?.invalidate()
        meter = nil
        recorder?.delegate = nil
        recorder?.stop()
        recorder = nil
        deactivateAudio()
    }

    private func deactivateAudio() {
        if ownsAudioSession {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            ownsAudioSession = false
        }
    }

    func audioRecorderDidFinishRecording(_ recorder: AVAudioRecorder, successfully flag: Bool) {
        elapsed = max(elapsed, recorder.currentTime)
        if elapsed > 299 { elapsed = 300 }
        stopCapture()
        phase = .paused
        message = flag ? nil : "Recording stopped unexpectedly. Tap ↑ to transcribe what was recorded."
        onUpdate?()
    }

    func audioRecorderEncodeErrorDidOccur(_ recorder: AVAudioRecorder, error: Error?) {
        failToStart("The recording could not be saved. Please try again.")
    }

    private func temporaryURL(_ ext: String) -> URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("zeron-dictation-\(UUID().uuidString).\(ext)")
    }

    private func cleanupFile() {
        if let recordingURL { try? FileManager.default.removeItem(at: recordingURL) }
        recordingURL = nil
    }

    deinit {
        task?.cancel()
        meter?.invalidate()
        recorder?.stop()
        if ownsAudioSession { try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation) }
        if let recordingURL { try? FileManager.default.removeItem(at: recordingURL) }
        observers.forEach { NotificationCenter.default.removeObserver($0) }
    }

    private struct RecordingError: LocalizedError {
        let message: String
        init(_ message: String) { self.message = message }
        var errorDescription: String? { message }
    }
}
