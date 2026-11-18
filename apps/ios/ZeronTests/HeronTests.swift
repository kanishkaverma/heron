import UIKit
import XCTest
@testable import Zeron

/// How Heron finds its one chat, wherever it runs.
final class HeronTests: XCTestCase {
    private typealias D = HeronWorld.Device
    private typealias C = HeronWorld.Chat

    private let air = D(id: "dev-air", name: "air", online: true)
    private let zeph = D(id: "dev-zeph", name: "zeph", online: false)

    private func chat(_ id: String, title: String = "Heron", cwd: String? = "~/heron", device: String = "dev-air",
                      parent: String? = nil, archived: Bool = false, created: Int64 = 1) -> C {
        C(id: id, title: title, cwd: cwd, deviceId: device, parentId: parent, archived: archived, createdAtMs: created)
    }

    func testWaitsForTheWorkspace() {
        XCTAssertEqual(HeronStep.next(HeronWorld(synced: false, devices: [air], chats: []), savedChat: nil), .waitForSync)
    }

    /// No chat yet: a computer's heron-keeper makes it; the app says whether one is up.
    func testWaitsForAComputerToMakeTheChat() {
        XCTAssertEqual(HeronStep.next(HeronWorld(synced: true, devices: [air, zeph], chats: []), savedChat: nil), .waitForChat(online: true))
        XCTAssertEqual(HeronStep.next(HeronWorld(synced: true, devices: [zeph], chats: []), savedChat: nil), .waitForChat(online: false))
    }

    /// The chat is the same one whichever computer runs it now.
    func testFollowsTheChatToAnotherComputer() {
        let world = HeronWorld(synced: true, devices: [air, zeph], chats: [chat("c-heron", device: "dev-zeph")])
        XCTAssertEqual(HeronStep.next(world, savedChat: "c-heron"), .open(chatId: "c-heron", deviceId: "dev-zeph", unarchive: false))
    }

    /// Only the top-level chat in ~/heron titled Heron counts; the original
    /// (oldest) wins, live before archived.
    func testPicksTheOneHeronChat() {
        let world = HeronWorld(synced: true, devices: [air], chats: [
            chat("c-sub", parent: "c-heron", created: 0),
            chat("c-code", title: "Fix the build", cwd: "~/code", created: 0),
            chat("c-legacy", cwd: "/Users/me/heron", created: 0),
            chat("c-old-archived", archived: true, created: 1),
            chat("c-heron", created: 5),
            chat("c-newer", created: 9),
        ])
        XCTAssertEqual(HeronStep.next(world, savedChat: nil), .open(chatId: "c-heron", deviceId: "dev-air", unarchive: false))
        XCTAssertEqual(HeronStep.next(world, savedChat: "c-newer"), .open(chatId: "c-newer", deviceId: "dev-air", unarchive: false))
        XCTAssertEqual(HeronStep.next(world, savedChat: "c-code"), .open(chatId: "c-heron", deviceId: "dev-air", unarchive: false))
    }

    func testBringsBackAnArchivedHeron() {
        let world = HeronWorld(synced: true, devices: [air], chats: [chat("c-heron", archived: true)])
        XCTAssertEqual(HeronStep.next(world, savedChat: nil), .open(chatId: "c-heron", deviceId: "dev-air", unarchive: true))
    }

    func testHeronFolderPaths() {
        XCTAssertTrue(Heron.isHeronFolder("/Users/me/heron"))
        XCTAssertTrue(Heron.isHeronFolder("/home/me/Heron/"))
        XCTAssertFalse(Heron.isHeronFolder("/Users/me/heron-notes"))
        XCTAssertEqual(Heron.folder(inHome: "/Users/me/"), "/Users/me/heron")
    }

    /// While Heron works, tapping send steers the message into the running
    /// turn rather than queueing it behind the whole turn; a harness that
    /// can't steer mid-turn still queues.
    @MainActor
    func testSendWhileWorkingSteers() {
        XCTAssertEqual(Heron.busyDelivery, .steer)
        XCTAssertEqual(sendWhileWorking(canSteer: true), .steer)
        XCTAssertEqual(sendWhileWorking(canSteer: false), .queue)
    }

    @MainActor
    private func sendWhileWorking(canSteer: Bool) -> DeliveryMode? {
        let composer = ComposerBar()
        composer.preferredDelivery = Heron.busyDelivery
        composer.running = true
        composer.canSteer = canSteer
        composer.text = "One more thing"
        var sent: DeliveryMode?
        composer.onSend = { _, _, mode in sent = mode; return .sent }
        let button = Self.find(in: composer) { $0.accessibilityIdentifier == "composer-send" } as? UIButton
        XCTAssertEqual(button?.accessibilityLabel, canSteer ? "Steer" : "Queue message")
        button?.sendActions(for: .touchUpInside)
        return sent
    }

    private static func find(in view: UIView, _ match: (UIView) -> Bool) -> UIView? {
        if match(view) { return view }
        for sub in view.subviews { if let hit = find(in: sub, match) { return hit } }
        return nil
    }
}
