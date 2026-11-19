import XCTest

/// The composer's mic in the Heron chat (offline demo). The live tests need a
/// speech sample and, for the cloud path, a Groq key from the test runner's
/// environment (TEST_RUNNER_ZERON_DICTATION_TEST_GROQ_KEY); no key belongs in
/// sources, launch arguments or result bundles.
final class DictationFlowTests: XCTestCase {
    override func setUp() { continueAfterFailure = false }

    private var env: [String: String] { ProcessInfo.processInfo.environment }

    private func launch(audio: String? = nil, key: String? = nil) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-fast"]
        if let audio { app.launchEnvironment["ZERON_DICTATION_TEST_AUDIO"] = audio }
        if let key { app.launchEnvironment["ZERON_DICTATION_TEST_GROQ_KEY"] = key }
        app.launch()
        return app
    }

    private func shot(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = env["DICTATION_SHOTS"] {
            try? app.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: "\(dir)/\(name).png"))
        }
    }

    func testMicIsVisibleInCompactAndExpandedComposer() {
        let app = launch()
        let mic = app.buttons["composer-dictation"]
        XCTAssertTrue(mic.waitForExistence(timeout: 15))
        XCTAssertTrue(mic.isHittable)
        app.textViews["composer-input"].tap()
        XCTAssertTrue(mic.isHittable)
        shot(app, "dictation-mic-focused")
    }

    func testHoldingTheMicOpensSettings() {
        let app = launch()
        let mic = app.buttons["composer-dictation"]
        XCTAssertTrue(mic.waitForExistence(timeout: 15))
        mic.press(forDuration: 1.0)
        let item = app.buttons["Voice to Text Settings"]
        XCTAssertTrue(item.waitForExistence(timeout: 5))
        item.tap()
        XCTAssertTrue(app.secureTextFields["dictation-api-key"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.textViews["dictation-vocabulary"].exists)
        shot(app, "dictation-settings")
    }

    /// The recording bar replaces the composer; ✕ restores the untouched draft.
    func testCancelRecordingPreservesDraft() throws {
        let path = env["DICTATION_TEST_AUDIO"] ?? "/tmp/heron-dict-heldout/h7.wav"
        guard FileManager.default.fileExists(atPath: path) else { throw XCTSkip("No speech sample at \(path)") }
        let app = launch(audio: path)
        let input = app.textViews["composer-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 15))
        input.tap()
        input.typeText("Keep this draft.")
        let before = input.value as? String
        app.buttons["composer-dictation"].tap()
        let cancel = app.buttons["dictation-cancel"]
        XCTAssertTrue(cancel.waitForExistence(timeout: 5))
        XCTAssertFalse(input.isHittable, "recording replaces the composer inline")
        sleep(1)
        shot(app, "dictation-recording")
        app.buttons["dictation-stop"].tap()
        XCTAssertTrue(app.buttons["Resume dictation"].waitForExistence(timeout: 3))
        shot(app, "dictation-paused")
        cancel.tap()
        XCTAssertTrue(cancel.waitForNonExistence(timeout: 5))
        XCTAssertEqual(input.value as? String, before)
    }

    /// Real speech through the real pipeline (Groq + on-device) into an unsent draft.
    func testLiveTranscriptionEditsDraftWithoutSending() throws {
        let path = env["DICTATION_TEST_AUDIO"] ?? "/tmp/heron-dict-heldout/h7.wav"
        guard FileManager.default.fileExists(atPath: path), let key = env["ZERON_DICTATION_TEST_GROQ_KEY"], !key.isEmpty else {
            throw XCTSkip("Needs a speech sample and TEST_RUNNER_ZERON_DICTATION_TEST_GROQ_KEY")
        }
        let app = launch(audio: path, key: key)
        let input = app.textViews["composer-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 15))
        input.tap()
        input.typeText("Note: ")
        let before = input.value as? String ?? ""
        app.buttons["composer-dictation"].tap()
        XCTAssertTrue(app.buttons["dictation-transcribe"].waitForExistence(timeout: 5))
        sleep(1)
        app.buttons["dictation-transcribe"].tap()
        XCTAssertTrue(app.staticTexts["dictation-status"].waitForExistence(timeout: 3))
        shot(app, "dictation-transcribing")
        XCTAssertTrue(app.buttons["dictation-cancel"].waitForNonExistence(timeout: 95))
        let text = input.value as? String ?? ""
        XCTAssertTrue(text.hasPrefix(before), "the transcript goes where the cursor was")
        let expected = (env["DICTATION_TEST_EXPECT"] ?? "calendar,Friday").split(separator: ",")
        for word in expected { XCTAssertTrue(text.lowercased().contains(word.lowercased()), "\(word) missing from: \(text)") }
        XCTAssertTrue(app.buttons["composer-send"].isEnabled, "the transcript stays an unsent draft")
        shot(app, "dictation-transcript-in-draft")
        print("DICTATION_UI_TRANSCRIPT\t\(text)")
    }
}
