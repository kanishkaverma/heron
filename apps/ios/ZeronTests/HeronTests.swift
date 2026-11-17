import XCTest
@testable import Zeron

/// How Heron finds its one chat: which computer, which folder, which chat,
/// and when to make them.
final class HeronTests: XCTestCase {
    private typealias D = HeronWorld.Device
    private typealias P = HeronWorld.Project
    private typealias C = HeronWorld.Chat

    private let air = D(id: "dev-air", name: "air", online: true)
    private let mac = D(id: "dev-mac", name: "MacBook Pro", online: false)

    private func chat(_ id: String, title: String = "Heron", project: String? = "p-heron", device: String = "dev-air",
                      parent: String? = nil, archived: Bool = false, created: Int64 = 1) -> C {
        C(id: id, title: title, projectId: project, deviceId: device, parentId: parent, archived: archived, createdAtMs: created)
    }

    private let heronProject = P(id: "p-heron", path: "/Users/me/heron", deviceId: "dev-air")

    func testWaitsForTheWorkspace() {
        let world = HeronWorld(synced: false, devices: [air], projects: [], chats: [])
        XCTAssertEqual(HeronStep.next(world, savedDevice: nil, savedChat: nil), .waitForSync)
    }

    func testAsksWhichComputerWhenThereAreSeveral() {
        let world = HeronWorld(synced: true, devices: [air, mac], projects: [], chats: [])
        XCTAssertEqual(HeronStep.next(world, savedDevice: nil, savedChat: nil), .pickComputer([air, mac]))
        // A remembered computer that's gone asks again.
        XCTAssertEqual(HeronStep.next(world, savedDevice: "dev-gone", savedChat: nil), .pickComputer([air, mac]))
    }

    /// Even one computer is asked about: right after signing in the list may
    /// still be arriving.
    func testAsksEvenWithOneComputer() {
        let world = HeronWorld(synced: true, devices: [air], projects: [], chats: [])
        XCTAssertEqual(HeronStep.next(world, savedDevice: nil, savedChat: nil), .pickComputer([air]))
        XCTAssertEqual(HeronStep.next(world, savedDevice: "dev-air", savedChat: nil), .createProject(deviceId: "dev-air"))
    }

    func testMakesTheChatInTheHeronFolder() {
        let other = P(id: "p-code", path: "/Users/me/code", deviceId: "dev-air")
        let world = HeronWorld(synced: true, devices: [air, mac], projects: [other, heronProject],
                               chats: [chat("c-code", title: "Fix the build", project: "p-code")])
        XCTAssertEqual(HeronStep.next(world, savedDevice: "dev-air", savedChat: nil), .createChat(projectId: "p-heron", deviceId: "dev-air"))
    }

    func testOpensTheRememberedChat() {
        let world = HeronWorld(synced: true, devices: [air, mac], projects: [heronProject],
                               chats: [chat("c-old", created: 1), chat("c-mine", created: 5)])
        XCTAssertEqual(HeronStep.next(world, savedDevice: "dev-air", savedChat: "c-mine"), .open(chatId: "c-mine", deviceId: "dev-air", unarchive: false))
    }

    /// A reinstall (nothing remembered) finds the chat that's already there,
    /// never a subagent's or a side chat's, and brings an archived one back.
    func testFindsTheExistingChatAfterAReinstall() {
        let world = HeronWorld(synced: true, devices: [air], projects: [heronProject], chats: [
            chat("c-sub", title: "Background: inbox triage", parent: "c-heron", created: 9),
            chat("c-other", title: "Something else", created: 0),
            chat("c-heron", archived: true, created: 3),
        ])
        XCTAssertEqual(HeronStep.next(world, savedDevice: "dev-air", savedChat: nil), .open(chatId: "c-heron", deviceId: "dev-air", unarchive: true))
    }

    func testChatsOnAnotherComputerDontCount() {
        let macProject = P(id: "p-mac-heron", path: "/Users/me/heron/", deviceId: "dev-mac")
        let world = HeronWorld(synced: true, devices: [air, mac], projects: [macProject, heronProject],
                               chats: [chat("c-mac", project: "p-mac-heron", device: "dev-mac")])
        XCTAssertEqual(HeronStep.next(world, savedDevice: "dev-air", savedChat: "c-mac"), .createChat(projectId: "p-heron", deviceId: "dev-air"))
    }

    func testPinsTheFirstOfferedPreferredModel() {
        XCTAssertEqual(Heron.pick(from: ["anthropic/claude-opus-5-5", "claude-bridge/claude-opus-5-5"]), "claude-bridge/claude-opus-5-5")
        XCTAssertEqual(Heron.pick(from: ["openai/gpt-5", "anthropic/claude-opus-5-5"]), "anthropic/claude-opus-5-5")
        XCTAssertNil(Heron.pick(from: ["openai/gpt-5"]))
    }

    func testHeronFolderPaths() {
        XCTAssertTrue(Heron.isHeronFolder("/Users/me/heron"))
        XCTAssertTrue(Heron.isHeronFolder("/home/me/Heron/"))
        XCTAssertFalse(Heron.isHeronFolder("/Users/me/heron-notes"))
        XCTAssertEqual(Heron.folder(inHome: "/Users/me/"), "/Users/me/heron")
    }
}
