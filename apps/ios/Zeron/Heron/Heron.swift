import Foundation

/// Heron: this build of the app is a one-chat personal assistant.
///
/// After signing in there is no session list, new-session page, side chats,
/// terminals or subagent views. There is one chat, titled "Heron", whose
/// agent (Pi) runs on one computer in that computer's `~/heron` folder. The
/// folder's AGENTS.md and Pi extension make the agent Heron: it keeps a
/// memory, uses your connected accounts (Gmail, Calendar, …) and hands
/// longer work to background threads you never see; it reports back here.
enum Heron {
    /// Heron's chrome instead of Zeron's (always on in this fork; one switch
    /// so the upstream code paths stay readable beside it).
    static let enabled = true

    /// The folder the chat runs in, under the computer's home.
    static let folderName = "heron"
    static let chatTitle = "Heron"
    /// The agent harness. The folder's AGENTS.md and Pi extension make it Heron.
    static let harness = "pi"
    /// What tapping send does while Heron is working: steer the message into
    /// the running turn, so the agent reads it at its next step (Pi takes it
    /// once the current tool calls return) instead of after the whole turn.
    /// Heron hands long work to background threads and should take the next
    /// request while it works. The send button's long-press still offers
    /// "Queue for next turn" and "Stop and send"; a message with images, or a
    /// harness that can't steer mid-turn, still queues.
    static let busyDelivery: DeliveryMode = .steer

    // MARK: Remembered (this phone)

    /// The Heron chat this phone last opened.
    static var chatId: String? {
        get { UserDefaults.standard.string(forKey: "heron.chatId") }
        set { UserDefaults.standard.set(newValue, forKey: "heron.chatId") }
    }

    /// Whether `path` is a Heron folder (`…/heron`).
    static func isHeronFolder(_ path: String) -> Bool {
        var p = path
        while p.count > 1, p.hasSuffix("/") { p.removeLast() }
        return (p as NSString).lastPathComponent.lowercased() == folderName
    }

    /// `home` + `/heron`.
    static func folder(inHome home: String) -> String {
        var h = home
        while h.count > 1, h.hasSuffix("/") { h.removeLast() }
        return h + "/" + folderName
    }
}

/// What the app knows of the workspace, reduced to what Heron needs.
struct HeronWorld: Equatable {
    struct Device: Equatable {
        let id: String
        let name: String
        let online: Bool
    }

    struct Chat: Equatable {
        let id: String
        let title: String
        let cwd: String?
        let deviceId: String
        let parentId: String?
        let archived: Bool
        let createdAtMs: Int64
    }

    /// The registry reached this phone at least once (lists are real).
    var synced: Bool
    /// Computers that can run agents.
    var devices: [Device]
    /// Every chat, archived ones included.
    var chats: [Chat]
}

/// The next thing to do to get to the Heron chat.
enum HeronStep: Equatable {
    /// The workspace hasn't arrived yet.
    case waitForSync
    /// No Heron chat yet: the computers' heron-keeper makes one. `online`:
    /// whether any computer is up to do it.
    case waitForChat(online: Bool)
    /// Open it (bringing it back first if it was archived).
    case open(chatId: String, deviceId: String, unarchive: Bool)

    /// The Heron chat is the one conversation, wherever it runs: top-level,
    /// working in `~/heron` (which every Heron computer has), titled Heron.
    /// heron-keeper moves it to an online computer when its own goes down.
    static func isHeronChat(_ c: HeronWorld.Chat) -> Bool {
        c.parentId == nil && c.title == Heron.chatTitle && ["~/heron", "~/heron/"].contains(c.cwd ?? "")
    }

    static func next(_ world: HeronWorld, savedChat: String?) -> HeronStep {
        guard world.synced else { return .waitForSync }
        let herons = world.chats.filter(isHeronChat)
        // The one this phone opened before, while it's still a Heron chat;
        // otherwise the live one first, the oldest (the original) first.
        let pick = herons.first { $0.id == savedChat && !$0.archived }
            ?? herons.sorted { ($0.archived ? 1 : 0, $0.createdAtMs) < ($1.archived ? 1 : 0, $1.createdAtMs) }.first
        guard let chat = pick else {
            return .waitForChat(online: world.devices.contains(where: \.online))
        }
        return .open(chatId: chat.id, deviceId: chat.deviceId, unarchive: chat.archived)
    }
}
