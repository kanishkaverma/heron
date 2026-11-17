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

    // MARK: Remembered choices (this phone)

    private static let deviceKey = "heron.deviceId"

    /// The computer Heron runs on.
    static var deviceId: String? {
        get { UserDefaults.standard.string(forKey: deviceKey) }
        set { UserDefaults.standard.set(newValue, forKey: deviceKey) }
    }

    static func chatId(on device: String) -> String? {
        UserDefaults.standard.string(forKey: "heron.chat.\(device)")
    }

    static func setChatId(_ id: String?, on device: String) {
        UserDefaults.standard.set(id, forKey: "heron.chat.\(device)")
    }

    static func forget() {
        if let device = deviceId { setChatId(nil, on: device) }
        deviceId = nil
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

    struct Project: Equatable {
        let id: String
        let path: String
        let deviceId: String
    }

    struct Chat: Equatable {
        let id: String
        let title: String
        let projectId: String?
        let deviceId: String
        let parentId: String?
        let archived: Bool
        let createdAtMs: Int64
    }

    /// The registry reached this phone at least once (lists are real).
    var synced: Bool
    /// Computers that can run agents.
    var devices: [Device]
    var projects: [Project]
    /// Every chat, archived ones included.
    var chats: [Chat]
}

/// The next thing to do to get to the Heron chat.
enum HeronStep: Equatable {
    /// The workspace hasn't arrived yet.
    case waitForSync
    /// No computer chosen (or the chosen one is gone): ask which.
    case pickComputer([HeronWorld.Device])
    /// The chat exists: open it (bringing it back first if it was archived).
    case open(chatId: String, deviceId: String, unarchive: Bool)
    /// The Heron folder is a project there, but there's no chat yet.
    case createChat(projectId: String, deviceId: String)
    /// The computer has no Heron project yet.
    case createProject(deviceId: String)

    /// Decide from what's known. `savedDevice`/`savedChat` are this phone's
    /// remembered choices.
    static func next(_ world: HeronWorld, savedDevice: String?, savedChat: String?) -> HeronStep {
        guard world.synced else { return .waitForSync }
        let device: String
        if let saved = savedDevice, world.devices.contains(where: { $0.id == saved }) {
            device = saved
        } else if world.devices.count == 1 {
            device = world.devices[0].id
        } else {
            return .pickComputer(world.devices)
        }
        // The chat this phone made or opened before.
        if let saved = savedChat, let chat = world.chats.first(where: { $0.id == saved && $0.deviceId == device && $0.parentId == nil }) {
            return .open(chatId: chat.id, deviceId: device, unarchive: chat.archived)
        }
        let projects = world.projects.filter { $0.deviceId == device && Heron.isHeronFolder($0.path) }
        let projectIds = Set(projects.map(\.id))
        // Another phone's (or a reinstall's) Heron chat in that folder:
        // titled Heron first, then any top-level chat there; live before
        // archived; the oldest — the original — first.
        let candidates = world.chats.filter { chat in
            chat.deviceId == device && chat.parentId == nil && chat.projectId.map(projectIds.contains) == true
        }
        let best = candidates.sorted { a, b in
            let ka = (a.title == Heron.chatTitle ? 0 : 1, a.archived ? 1 : 0, a.createdAtMs)
            let kb = (b.title == Heron.chatTitle ? 0 : 1, b.archived ? 1 : 0, b.createdAtMs)
            return ka < kb
        }.first
        if let best {
            return .open(chatId: best.id, deviceId: device, unarchive: best.archived)
        }
        if let project = projects.first {
            return .createChat(projectId: project.id, deviceId: device)
        }
        return .createProject(deviceId: device)
    }
}
