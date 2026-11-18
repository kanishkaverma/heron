import XCTest

/// Heron on the Rust core's offline demo workspace: the app opens straight
/// into one chat (no session list, new-session button, companions or voice),
/// sends, and the host streams a reply.
final class HeronFlowTests: XCTestCase {
    override func setUp() {
        continueAfterFailure = false
    }

    private func snapshot(_ app: XCUIApplication, _ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }

    func testOpensStraightIntoTheOneChat() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-fast", "-fresh-demo"]
        app.launch()
        let input = app.textViews["composer-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 20), "the chat's composer, with no list in front of it")
        let titled = app.navigationBars["Heron"].exists || app.navigationBars.staticTexts["Heron"].exists
        XCTAssertTrue(titled, "titled Heron")
        // Nothing but the chat.
        XCTAssertFalse(app.buttons["new-session"].exists)
        XCTAssertFalse(app.buttons["bottom-settings"].exists)
        XCTAssertFalse(app.buttons["session-companions"].exists)
        XCTAssertFalse(app.staticTexts["Sessions"].exists)
        XCTAssertFalse(app.navigationBars.buttons["BackButton"].exists)
        snapshot(app, "1-heron-chat")

        input.tap()
        input.typeText("What's on my calendar tomorrow?")
        snapshot(app, "2-typing")
        app.buttons["composer-send"].tap()
        let echo = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'You: What'")).firstMatch
        XCTAssertTrue(echo.waitForExistence(timeout: 5), "the message is in the chat")
        let reply = app.staticTexts.matching(identifier: "row-markdown").element(boundBy: 0)
        XCTAssertTrue(reply.waitForExistence(timeout: 20), "a reply streams in")
        XCTAssertTrue(app.buttons["Send message"].waitForExistence(timeout: 30), "the turn settles")
        snapshot(app, "3-reply")

        // The ⋯ menu: the computer, the transcript, signing out; no side chats or terminals.
        app.buttons["session-menu"].tap()
        XCTAssertTrue(app.buttons["Copy Transcript"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Sign Out"].exists)
        XCTAssertFalse(app.buttons["New Side Chat"].exists)
        XCTAssertFalse(app.buttons["Terminal"].exists)
        snapshot(app, "4-menu")
    }

    /// A message sent while Heron works goes into the running turn (steer),
    /// not into a queue that waits for the whole turn to end.
    func testSendWhileWorkingSteersInsteadOfQueueing() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-fresh-demo", "-longreply"]
        app.launch()
        let input = app.textViews["composer-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 20))
        input.tap()
        input.typeText("Write a long reply please.")
        app.buttons["composer-send"].tap()
        XCTAssertTrue(app.buttons["Stop response"].waitForExistence(timeout: 5), "Heron is working")
        input.tap()
        input.typeText("Also check tomorrow")
        XCTAssertTrue(app.buttons["Steer"].waitForExistence(timeout: 2), "send steers while Heron works")
        app.buttons["Steer"].tap()
        let echo = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'You: Also check'")).firstMatch
        XCTAssertTrue(echo.waitForExistence(timeout: 5), "the message is in the chat at once")
        XCTAssertFalse(app.buttons["More queue actions"].exists, "nothing waits in the queue")
        snapshot(app, "5-steered")
    }

    /// Regression: the chat is made without an effort, and the effort chip
    /// only showed one already set, so it could never be changed.
    func testThinkingLevelCanBeChanged() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-fast", "-fresh-demo"]
        app.launch()
        let input = app.textViews["composer-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 20))
        input.tap()
        let chip = app.buttons["composer-chip-effort"]
        XCTAssertTrue(chip.waitForExistence(timeout: 20), "an effort chip on a chat without one")
        XCTAssertTrue(chip.label.contains("Default"), "got \(chip.label)")
        snapshot(app, "5-effort-default")
        chip.tap()
        let high = app.buttons["High"].firstMatch
        XCTAssertTrue(high.waitForExistence(timeout: 5), "the model's levels load")
        snapshot(app, "6-effort-menu")
        high.tap()
        XCTAssertTrue(app.buttons["composer-chip-effort"].waitForLabel(containing: "High", timeout: 5))
        snapshot(app, "7-effort-high")
        // (The demo reseeds its chats on launch, so keeping it across a
        // relaunch is the host's job, not something this can check.)
    }
}

private extension XCUIElement {
    func waitForLabel(containing text: String, timeout: TimeInterval) -> Bool {
        let match = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", text), object: self)
        return XCTWaiter.wait(for: [match], timeout: timeout) == .completed
    }
}
