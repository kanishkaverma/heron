import XCTest
#if canImport(UIKit)
import AVFoundation
import UIKit
@testable import Zeron
#else
@testable import Dictation
#endif

final class DictationTests: XCTestCase {
    private func client(status: Int = 200, response: String, inspect: ((URLRequest) -> Void)? = nil) -> GroqTranscription {
        DictationURLProtocol.handler = { request in
            inspect?(request)
            return (status, Data(response.utf8))
        }
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [DictationURLProtocol.self]
        return GroqTranscription(session: URLSession(configuration: config))
    }

    func testUploadsAudioToWhisperLargeV3AndReturnsTrimmedText() async throws {
        let api = client(response: "{\"text\":\"  Hello from dictation.  \"}") { request in
            XCTAssertEqual(request.url?.absoluteString, "https://api.groq.com/openai/v1/audio/transcriptions")
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer test-only")
            // URLSession can materialize httpBody as a stream at this boundary.
            var data = request.httpBody ?? Data()
            if let stream = request.httpBodyStream {
                stream.open(); defer { stream.close() }
                var buffer = [UInt8](repeating: 0, count: 4096)
                while stream.hasBytesAvailable {
                    let n = stream.read(&buffer, maxLength: buffer.count)
                    if n <= 0 { break }
                    data.append(contentsOf: buffer.prefix(n))
                }
            }
            let body = String(decoding: data, as: UTF8.self)
            XCTAssertTrue(body.contains("\r\n\r\nwhisper-large-v3\r\n"), "the full model, not Turbo")
            XCTAssertFalse(body.contains("name=\"language\""), "language is detected, not forced")
            XCTAssertFalse(body.contains("name=\"prompt\""), "a prompt made Whisper echo it back in testing")
            XCTAssertTrue(body.contains("name=\"file\"; filename=\"recording.m4a\""))
            XCTAssertTrue(body.contains("known audio bytes"))
        }
        let text = try await api.transcribe(audio: Data("known audio bytes".utf8), key: " test-only\n")
        XCTAssertEqual(text, "Hello from dictation.")
    }

    func testConnectionChecksSelectedModel() async throws {
        let api = client(response: "{\"id\":\"whisper-large-v3\"}") { request in
            XCTAssertEqual(request.url?.path, "/openai/v1/models/whisper-large-v3")
            XCTAssertEqual(request.httpMethod, "GET")
        }
        try await api.testKey("test-only")
    }

    func testAuthenticationFailureDoesNotExposeKeyOrServerBody() async {
        let api = client(status: 401, response: "secret key echoed by server")
        do {
            try await api.testKey("test-only")
            XCTFail("Invalid key must fail")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("rejected"))
            XCTAssertFalse(error.localizedDescription.contains("test-only"))
            XCTAssertFalse(error.localizedDescription.contains("echoed"))
        }
    }

    func testRateLimitIsActionable() async {
        let api = client(status: 429, response: "{}")
        do {
            _ = try await api.transcribe(audio: Data([1]), key: "test-only")
            XCTFail("Rate limit must fail")
        } catch { XCTAssertTrue(error.localizedDescription.contains("limit")) }
    }

    func testEmptyTranscriptDoesNotBecomeADraft() async {
        let api = client(response: "{\"text\":\"   \"}")
        do {
            _ = try await api.transcribe(audio: Data([1]), key: "test-only")
            XCTFail("Empty speech must fail")
        } catch { XCTAssertTrue(error.localizedDescription.contains("No speech")) }
    }

    func testMissingKeyAndEmptyAudioAreRejectedLocally() async {
        do { try await GroqTranscription().testKey(""); XCTFail("Missing key") }
        catch { XCTAssertTrue(error.localizedDescription.contains("API key")) }
        do { _ = try await GroqTranscription().transcribe(audio: Data(), key: "test"); XCTFail("Empty audio") }
        catch { XCTAssertTrue(error.localizedDescription.contains("empty")) }
    }

    // MARK: Pipeline (Whisper + on-device, merged on Groq)

    private func audioFile() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("dictation-test-\(UUID().uuidString).wav")
        try Data("known audio bytes".utf8).write(to: url)
        addTeardownBlock { try? FileManager.default.removeItem(at: url) }
        return url
    }

    /// Routes by endpoint: Whisper, then the cleanup chat.
    private func pipeline(key: String = "test-only", whisper: (Int, String), chat: (Int, String)? = nil,
                          onDevice: @escaping (URL, [String]) async throws -> String,
                          inspectChat: ((String) -> Void)? = nil) -> DictationPipeline {
        DictationURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/chat/completions") == true {
                inspectChat?(Self.body(of: request))
                return chat.map { ($0.0, Data($0.1.utf8)) } ?? (500, Data())
            }
            return (whisper.0, Data(whisper.1.utf8))
        }
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [DictationURLProtocol.self]
        var p = DictationPipeline(key: key, vocabulary: ["Heron", "heronctl"])
        p.groq = GroqTranscription(session: URLSession(configuration: config))
        p.onDevice = onDevice
        return p
    }

    private static func body(of request: URLRequest) -> String {
        var data = request.httpBody ?? Data()
        if let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let n = stream.read(&buffer, maxLength: buffer.count)
                if n <= 0 { break }
                data.append(contentsOf: buffer.prefix(n))
            }
        }
        return String(decoding: data, as: UTF8.self)
    }

    private static func chatReply(_ text: String) -> String {
        let json = try! JSONSerialization.data(withJSONObject: ["choices": [["message": ["content": text]]]])
        return String(decoding: json, as: UTF8.self)
    }

    func testWithKeyWhisperIsCleanedUpAsTextOnly() async throws {
        var chatBody = ""
        let p = pipeline(whisper: (200, "{\"text\":\"यार, कल सुबह meeting है\"}"),
                         chat: (200, Self.chatReply("Yaar, kal subah meeting hai.")),
                         onDevice: { _, _ in XCTFail("the phone isn't needed when Groq works"); return "" },
                         inspectChat: { chatBody = $0 })
        let result = try await p.transcribe(fileURL: try audioFile())
        XCTAssertEqual(result, .init(text: "Yaar, kal subah meeting hai.", source: .merged))
        XCTAssertTrue(chatBody.contains("openai/gpt-oss-120b"))
        XCTAssertTrue(chatBody.contains("<whisper>यार, कल सुबह meeting है</whisper>"))
        XCTAssertTrue(chatBody.contains("heronctl"), "the person's spellings go with the cleanup pass")
        XCTAssertFalse(chatBody.contains("known audio bytes"), "audio never goes to the text model")
    }

    func testWithoutKeyOnlyTheDeviceTranscribes() async throws {
        let p = pipeline(key: "", whisper: (500, "should not be called"), onDevice: { _, _ in "Hello Heron" })
        DictationURLProtocol.handler = { _ in XCTFail("no network without a key"); return (500, Data()) }
        let result = try await p.transcribe(fileURL: try audioFile())
        XCTAssertEqual(result, .init(text: "Hello Heron", source: .onDevice))
    }

    func testFailedMergeFallsBackToWhisperOrRomanDevice() async throws {
        let english = pipeline(whisper: (200, "{\"text\":\"Deploy with heronctl.\"}"), chat: (500, "{}"), onDevice: { _, _ in "deploy with her until" })
        let r1 = try await english.transcribe(fileURL: try audioFile())
        XCTAssertEqual(r1, .init(text: "Deploy with heronctl.", source: .whisper))
        let hindi = pipeline(whisper: (200, "{\"text\":\"ठीक है\"}"), chat: (500, "{}"), onDevice: { _, _ in "theek hai" })
        let r2 = try await hindi.transcribe(fileURL: try audioFile())
        XCTAssertEqual(r2, .init(text: "theek hai", source: .onDevice), "Devanagari is not a draft he would type")
    }

    func testAMergeThatAnswersInsteadOfTranscribingIsRejected() async throws {
        let rambling = Array(repeating: "Sure, here is a long helpful answer.", count: 6).joined(separator: " ")
        let p = pipeline(whisper: (200, "{\"text\":\"What time is it?\"}"), chat: (200, Self.chatReply(rambling)), onDevice: { _, _ in "what time is it" })
        let result = try await p.transcribe(fileURL: try audioFile())
        XCTAssertEqual(result.source, .whisper)
        XCTAssertEqual(GroqTranscription.stripTags("<whisper>Hi</whisper>"), "Hi")
    }

    func testCloudDownStillUsesTheDevice() async throws {
        let p = pipeline(whisper: (429, "{}"), chat: (500, "{}"), onDevice: { _, hints in XCTAssertEqual(hints, ["Heron", "heronctl"]); return "offline words" })
        let result = try await p.transcribe(fileURL: try audioFile())
        XCTAssertEqual(result, .init(text: "offline words", source: .onDevice))
    }

    func testNothingWorksSaysWhy() async throws {
        struct DeviceDown: Error {}
        let p = pipeline(whisper: (429, "{}"), onDevice: { _, _ in throw DeviceDown() })
        do { _ = try await p.transcribe(fileURL: try audioFile()); XCTFail("must fail") }
        catch { XCTAssertTrue(error.localizedDescription.contains("limit"), error.localizedDescription) }
        let explained = DictationPipeline.explain(DeviceDown(), cloud: URLError(.notConnectedToInternet))
        XCTAssertTrue(explained.localizedDescription.contains("No internet"))
    }

    func testVocabularyParsing() {
        XCTAssertEqual(DictationVocabulary.parse(" Heron, nishka-server,\nUniFFI ,, "), ["Heron", "nishka-server", "UniFFI"])
    }

    /// Opt-in accuracy run over real speech through the production pipeline:
    /// DICTATION_LIVE_CLIPS=<dir with refs.tsv and <id>.wav> and GROQ_API_KEY.
    /// Prints each transcript, its word error rate and the source used.
    func testLiveAccuracyOnSpokenClips() async throws {
        let env = ProcessInfo.processInfo.environment
        guard let dir = env["DICTATION_LIVE_CLIPS"], let key = env["GROQ_API_KEY"], !key.isEmpty else {
            throw XCTSkip("Set DICTATION_LIVE_CLIPS and GROQ_API_KEY to run")
        }
        let refs = try String(contentsOfFile: dir + "/refs.tsv", encoding: .utf8).split(separator: "\n").map { $0.split(separator: "\t", maxSplits: 2).map(String.init) }
        var modes: [(String, DictationPipeline)] = [("pipeline", DictationPipeline(key: key, vocabulary: DictationVocabulary.starter))]
        if env["DICTATION_LIVE_ALL"] != nil {
            modes.append(("on-device only", DictationPipeline(key: "", vocabulary: DictationVocabulary.starter)))
            var raw = DictationPipeline(key: key, vocabulary: DictationVocabulary.starter)
            raw.cleanup = false
            modes.append(("no cleanup", raw))
        }
        for (name, p) in modes {
            var total = 0.0
            for ref in refs where ref.count == 3 {
                let url = URL(fileURLWithPath: "\(dir)/\(ref[0]).wav")
                let result = try await p.transcribe(fileURL: url)
                let w = Self.wer(ref[2], result.text)
                total += w
                print("LIVE\t\(name)\t\(ref[0])\t\(String(format: "%.3f", w))\t\(result.source.rawValue)\t\(result.text)")
                try await Task.sleep(for: .seconds(4)) // Groq's free tier allows ~20 requests a minute
            }
            let mean = total / Double(refs.count)
            print("LIVE\t\(name)\tMEAN\t\(String(format: "%.3f", mean))")
            if name == "pipeline" { XCTAssertLessThan(mean, 0.2) }
        }
    }

    static func wer(_ reference: String, _ hypothesis: String) -> Double {
        func words(_ s: String) -> [String] {
            s.lowercased().replacingOccurrences(of: "-", with: " ")
                .unicodeScalars.map { CharacterSet.alphanumerics.contains($0) ? String($0) : " " }.joined()
                .split(separator: " ").map(String.init)
        }
        let r = words(reference), h = words(hypothesis)
        guard !r.isEmpty else { return h.isEmpty ? 0 : 1 }
        var d = Array(0...h.count)
        for i in 1...r.count {
            var prev = d[0]; d[0] = i
            for j in stride(from: 1, through: h.count, by: 1) {
                let cur = d[j]
                d[j] = min(d[j] + 1, d[j - 1] + 1, prev + (r[i - 1] == h[j - 1] ? 0 : 1))
                prev = cur
            }
        }
        return Double(d[h.count]) / Double(r.count)
    }

    #if canImport(UIKit)
    @MainActor func testTranscriptInsertsAtCursorWithoutSendingOrRemovingImages() {
        let composer = ComposerBar()
        var sent = false
        composer.onSend = { _, _, _ in sent = true; return .sent }
        composer.images = [StagedImage(id: "photo", name: "photo.jpg", data: Data([1]), thumbnail: UIImage())]
        composer.text = "Please fix today"
        composer.textView.selectedRange = NSRange(location: 10, length: 0)
        composer.insertTranscript("the tests")
        XCTAssertEqual(composer.text, "Please fix the tests today")
        XCTAssertFalse(sent)
        XCTAssertEqual(composer.images.map(\.id), ["photo"])
    }

    @MainActor func testTranscriptReplacesSelectionAndSupportsUnicode() {
        let composer = ComposerBar()
        composer.text = "👋 old draft"
        composer.textView.selectedRange = NSRange(location: 3, length: 3)
        composer.insertTranscript("new")
        XCTAssertEqual(composer.text, "👋 new draft")
        composer.insertTranscript(" ")
        XCTAssertEqual(composer.text, "👋 new draft")
    }

    @MainActor func testKeyPersistsAndCanBeRemoved() throws {
        let original = GroqAPIKey.value
        defer { try? GroqAPIKey.save(original) }
        try GroqAPIKey.save(" test-only ")
        XCTAssertEqual(GroqAPIKey.value, "test-only")
        try GroqAPIKey.save("")
        XCTAssertEqual(GroqAPIKey.value, "")
    }

    @MainActor func testMicrophoneProducesAWAVFile() async throws {
        #if targetEnvironment(simulator)
        let allowed = await AVAudioApplication.requestRecordPermission()
        guard allowed else { throw XCTSkip("Simulator microphone permission not granted") }
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record)
        try session.setActive(true)
        defer { try? session.setActive(false) }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".wav")
        defer { try? FileManager.default.removeItem(at: url) }
        let recorder = try AVAudioRecorder(url: url, settings: [AVFormatIDKey: kAudioFormatLinearPCM, AVSampleRateKey: 16000, AVNumberOfChannelsKey: 1, AVLinearPCMBitDepthKey: 16])
        XCTAssertTrue(recorder.record())
        try await Task.sleep(for: .seconds(1))
        recorder.stop()
        let file = try Data(contentsOf: url)
        XCTAssertGreaterThan(file.count, 100)
        #endif
    }
    #endif
}

private final class DictationURLProtocol: URLProtocol {
    static var handler: ((URLRequest) -> (Int, Data))?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let handler = Self.handler else { return }
        let (status, data) = handler(request)
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}
