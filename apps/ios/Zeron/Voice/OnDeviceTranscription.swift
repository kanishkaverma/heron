import AVFoundation
import Foundation
import Speech

/// Apple's on-device model (SpeechAnalyzer + SpeechTranscriber, iOS 26+): what
/// T3 Code's iPhone app dictates with by default. It runs on the finished
/// recording, needs no key or network once its language model is installed, and
/// in Hindi (India) writes Hinglish in Roman script ("kal subah … bhej dena")
/// while keeping English words English.
struct OnDeviceTranscription {
    /// Tried in order: hi-IN handles Hinglish and Indian English best in our
    /// clips; the English locales cover phones without the Hindi model.
    static let locales = ["hi-IN", "en-IN", "en-US"]

    enum Failure: LocalizedError {
        case unavailable, unsupportedLocale, noSpeech
        var errorDescription: String? {
            switch self {
            case .unavailable: return "On-device transcription isn't available on this iPhone."
            case .unsupportedLocale: return "On-device transcription doesn't support your languages."
            case .noSpeech: return "No speech was transcribed. Please try again."
            }
        }
    }

    static var isAvailable: Bool { SpeechTranscriber.isAvailable }

    /// The first locale Apple supports, with its model installed (downloads it
    /// the first time, which needs a network connection once).
    static func prepare(_ candidates: [String] = locales) async throws -> Locale {
        guard SpeechTranscriber.isAvailable else { throw Failure.unavailable }
        for identifier in candidates {
            guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: identifier)) else { continue }
            let transcriber = SpeechTranscriber(locale: locale, preset: .transcription)
            if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                try await request.downloadAndInstall()
            }
            return locale
        }
        throw Failure.unsupportedLocale
    }

    /// Transcribes a finished recording. `hints` are names and terms to favour.
    static func transcribe(fileURL: URL, hints: [String] = [], locales: [String] = locales) async throws -> String {
        let locale = try await prepare(locales)
        let transcriber = SpeechTranscriber(locale: locale, preset: .transcription)
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        if !hints.isEmpty {
            let context = AnalysisContext()
            context.contextualStrings[.general] = hints
            try? await analyzer.setContext(context)
        }
        let file = try AVAudioFile(forReading: fileURL)
        let collect = Task {
            var text = ""
            for try await result in transcriber.results where result.isFinal {
                text += String(result.text.characters)
            }
            return text
        }
        do {
            if let last = try await analyzer.analyzeSequence(from: file) {
                try await analyzer.finalizeAndFinish(through: last)
            } else {
                await analyzer.cancelAndFinishNow()
            }
        } catch {
            collect.cancel()
            throw error
        }
        let text = try await collect.value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { throw Failure.noSpeech }
        return text
    }
}
