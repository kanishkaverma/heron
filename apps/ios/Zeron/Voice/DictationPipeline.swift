import Foundation

/// Turns a finished recording into draft text, as accurately as what's
/// available allows:
///
/// 1. With a Groq key: Whisper Large v3 in the cloud (language detected, so
///    Hindi comes back in Devanagari), then a text-only pass on Groq
///    (`openai/gpt-oss-120b`) writes Hinglish in Roman script and fixes the
///    person's own names and terms. See docs/ios-dictation.md for the numbers.
/// 2. If that pass fails: Whisper's text, or the on-device transcript when
///    Whisper's has Devanagari.
/// 3. Without a key, offline, or with Groq down: Apple's on-device model
///    (hi-IN), which is what T3 Code's iPhone app uses by default.
///
/// Nothing here sends a message: the caller only edits the draft.
struct DictationPipeline {
    enum Source: String { case merged, whisper, onDevice }
    struct Result: Equatable { let text: String; let source: Source }

    var key: String
    var vocabulary: [String]
    var groq = GroqTranscription()
    var onDevice: (URL, [String]) async throws -> String = { url, hints in
        try await OnDeviceTranscription.transcribe(fileURL: url, hints: hints)
    }
    var cleanup = true

    func transcribe(fileURL: URL) async throws -> Result {
        let hasKey = !key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        guard hasKey else {
            do { return Result(text: try await onDevice(fileURL, vocabulary), source: .onDevice) }
            catch { throw Self.explain(error, cloud: nil) }
        }
        let whisper: String
        do {
            let audio = try Data(contentsOf: fileURL)
            let ext = fileURL.pathExtension.lowercased()
            whisper = try await groq.transcribe(audio: audio, key: key, filename: "recording." + ext,
                                                mimeType: ext == "wav" ? "audio/wav" : "audio/mp4")
        } catch {
            if error is CancellationError || Task.isCancelled { throw CancellationError() }
            let cloudError = error
            Self.log("whisper failed: \(cloudError)")
            // Offline, rate-limited or Groq down: the phone can still do it.
            do { return Result(text: try await onDevice(fileURL, vocabulary), source: .onDevice) }
            catch {
                Self.log("on-device failed: \(error)")
                throw Self.explain(cloudError, cloud: cloudError)
            }
        }
        try Task.checkCancellation()
        if cleanup {
            // One retry: a free Groq key hits its per-minute limit easily.
            for attempt in 0..<2 {
                do {
                    let merged = try await groq.cleanUp(whisper: whisper, onDevice: nil, vocabulary: vocabulary, key: key)
                    if Self.plausible(merged, against: [whisper]) { return Result(text: merged, source: .merged) }
                    Self.log("cleanup rejected as implausible")
                    break
                } catch {
                    Self.log("cleanup failed: \(error)")
                    guard attempt == 0, case GroqTranscription.Failure.rateLimited = error else { break }
                    try await Task.sleep(for: .seconds(2))
                }
            }
            try Task.checkCancellation()
        }
        // Without the cleanup, Whisper writes Hindi in Devanagari: the
        // on-device Roman Hinglish is then the better draft.
        if Self.hasDevanagari(whisper), let device = try? await onDevice(fileURL, vocabulary) {
            return Result(text: device, source: .onDevice)
        }
        return Result(text: whisper, source: .whisper)
    }

    /// Error kinds only: never the key, never what was said.
    static func log(_ message: String) {
        #if DEBUG
        NSLog("[dictation] %@", message)
        #endif
    }

    static func hasDevanagari(_ text: String) -> Bool {
        text.unicodeScalars.contains { (0x0900...0x097F).contains($0.value) }
    }

    /// A merge that is far longer or shorter than its inputs answered or
    /// summarized instead of transcribing: fall back to the raw transcript.
    static func plausible(_ merged: String, against inputs: [String]) -> Bool {
        let words = { (s: String) in s.split(whereSeparator: { $0.isWhitespace }).count }
        guard let longest = inputs.map(words).max(), longest > 0 else { return false }
        let n = words(merged)
        return n > 0 && Double(n) <= Double(longest) * 1.6 + 3 && Double(n) >= Double(longest) * 0.5 - 2
    }

    /// Offline is the common failure: say so plainly.
    static func explain(_ error: Error, cloud: Error?) -> Error {
        if let urlError = (cloud ?? error) as? URLError,
           [.notConnectedToInternet, .networkConnectionLost, .timedOut, .cannotFindHost, .cannotConnectToHost, .dataNotAllowed].contains(urlError.code) {
            return Message("No internet connection, and on-device transcription couldn't run. Your recording is kept: tap ↑ to retry.")
        }
        return error
    }

    struct Message: LocalizedError {
        let text: String
        init(_ text: String) { self.text = text }
        var errorDescription: String? { text }
    }
}

/// Names and terms the person dictates often; sent with the cleanup pass and
/// to the on-device model as hints. Stored on this device only.
enum DictationVocabulary {
    static let defaultsKey = "dictation.vocabulary"
    static let starter = ["Heron", "Zeron", "heronctl", "nishka-server", "zeph", "Kanishka", "GitHub", "UIKit", "SwiftUI", "UniFFI", "Tailscale", "Groq", "Claude", "Codex"]
    static var terms: [String] {
        get { (UserDefaults.standard.string(forKey: defaultsKey)).map(parse) ?? starter }
        set { UserDefaults.standard.set(newValue.joined(separator: ", "), forKey: defaultsKey) }
    }
    static func parse(_ text: String) -> [String] {
        text.split(whereSeparator: { $0 == "," || $0 == "\n" }).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
    }
}
