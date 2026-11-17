import UIKit

/// The signed-in app: the one Heron chat, full screen. Until it's found (or
/// made) a status page says what's happening; with several computers and
/// none chosen it asks which one Heron lives on.
final class HeronRootController: UIViewController, AppRouter {
    private let app: AppModel
    private let nav = UINavigationController()
    private let status = HeronStatusViewController()
    private var appToken: AnyObject?
    private var poll: Timer?
    private var working = false
    private(set) var chatId: String?
    private var pendingComputer = Heron.launchComputer

    init(app: AppModel) {
        self.app = app
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        nav.navigationBar.titleTextAttributes = [.font: Fonts.ui(.sansSemibold, 17), .foregroundColor: Palette.text]
        nav.navigationBar.tintColor = Palette.text
        nav.setViewControllers([status], animated: false)
        addChild(nav)
        nav.view.frame = view.bounds
        nav.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(nav.view)
        nav.didMove(toParent: self)
        status.onPick = { [weak self] device in
            Heron.deviceId = device
            self?.resolve()
        }
        status.onRetry = { [weak self] in self?.resolve() }
        status.onChooseComputer = { [weak self] in
            guard let self else { return }
            Heron.deviceId = nil
            self.resolve()
        }
        status.onSignOut = { [weak self] in self?.app.signOut() }
        appToken = app.observe { [weak self] in self?.resolveIfWaiting() }
        resolve()
    }

    override var childForStatusBarStyle: UIViewController? { nav }

    // MARK: Finding the chat

    private func world() -> HeronWorld? {
        guard let client = app.client else { return nil }
        let ws = client.workspace()
        let devices = client.executionDevices().map { HeronWorld.Device(id: $0.id, name: $0.name, online: $0.online) }
        let projects = ws.projects.filter { !$0.isCloud }.map { HeronWorld.Project(id: $0.id, path: $0.path, deviceId: $0.deviceId) }
        var seen = Set<String>()
        var chats: [HeronWorld.Chat] = []
        func add(_ r: SessionRow) {
            guard seen.insert(r.id).inserted else { return }
            chats.append(HeronWorld.Chat(id: r.id, title: r.title, projectId: r.project?.id, deviceId: r.deviceId, parentId: r.parentChatId, archived: r.archived, createdAtMs: r.createdAtMs))
        }
        for p in ws.projects { p.sessions.forEach(add) }
        ws.archived.forEach(add)
        ws.projectless.forEach(add)
        return HeronWorld(synced: ws.synced, devices: devices, projects: projects, chats: chats)
    }

    /// Re-run while the status page is up (the workspace arriving, a
    /// computer coming online).
    private func resolveIfWaiting() {
        guard chatId == nil, !working else { return }
        resolve()
    }

    func resolve() {
        guard !working, chatId == nil else { return }
        if app.isDemo { return openDemo() }
        guard let world = world() else { return }
        // `-heron-computer <name or id>`: chosen on launch, once it's listed.
        if let wanted = pendingComputer?.lowercased(),
           let device = world.devices.first(where: { $0.id.lowercased() == wanted || $0.name.lowercased() == wanted }) {
            pendingComputer = nil
            if Heron.deviceId != device.id { Heron.deviceId = device.id }
        }
        let step = HeronStep.next(world, savedDevice: Heron.deviceId, savedChat: Heron.deviceId.flatMap(Heron.chatId(on:)))
        let deviceName = { (id: String) in world.devices.first { $0.id == id }?.name ?? "your computer" }
        switch step {
        case .waitForSync:
            status.show(.working("Connecting…"))
            keepPolling()
        case .pickComputer(let devices):
            stopPolling()
            if devices.isEmpty {
                status.show(.problem("No computer is signed in to this account yet. Install Zeron on your laptop, sign in with the same account, then try again."))
                keepPolling()
            } else {
                status.show(.pick(devices))
            }
        case let .open(chatId, deviceId, unarchive):
            Heron.deviceId = deviceId
            if unarchive { app.unarchive(chatId) }
            open(chatId, deviceId: deviceId)
        case let .createChat(projectId, deviceId):
            Heron.deviceId = deviceId
            status.show(.working("Setting up Heron on \(deviceName(deviceId))…"))
            createChat(projectId: projectId, deviceId: deviceId)
        case .createProject(let deviceId):
            Heron.deviceId = deviceId
            status.show(.working("Setting up Heron on \(deviceName(deviceId))…"))
            createProject(deviceId: deviceId, name: deviceName(deviceId))
        }
    }

    private func keepPolling() {
        guard poll == nil else { return }
        poll = Timer.scheduledTimer(withTimeInterval: 1.5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.resolveIfWaiting() }
        }
    }

    private func stopPolling() {
        poll?.invalidate()
        poll = nil
    }

    private func createProject(deviceId: String, name: String) {
        working = true
        Task { @MainActor in
            defer { working = false }
            guard let listing = await app.listFolders(deviceId: deviceId, path: nil) else {
                status.show(.problem("\(name) isn't answering. Make sure it's awake and Zeron is running on it."))
                keepPolling()
                return
            }
            let folder = Heron.folder(inHome: listing.path)
            // The folder holds who Heron is (AGENTS.md, its extension): no
            // folder, no Heron there yet.
            guard listing.entries.contains(where: { $0.isDir && $0.name.lowercased() == Heron.folderName }) else {
                status.show(.problem("Heron isn't set up on \(name) yet (no \(folder)). Run scripts/heron/setup-computer.sh there, or choose another computer."))
                return
            }
            guard await app.createProject(deviceId: deviceId, path: folder, gitDetected: false) != nil else {
                status.show(.problem("Couldn't use \(folder) on \(name). Create that folder (with Heron's AGENTS.md) and try again."))
                return
            }
            // The project shows up in the workspace a moment later.
            app.workspaceDidChange()
            try? await Task.sleep(for: .milliseconds(400))
            working = false
            resolve()
        }
    }

    private func createChat(projectId: String, deviceId: String) {
        guard let client = app.client else { return }
        let config = ChatConfig(harness: Heron.harness, model: nil, reasoning: nil, modelOptions: [:], sandbox: .dangerFullAccess)
        do {
            let id = try client.createSession(newSession: NewSession(target: .project(spaceId: projectId), config: config, branch: nil, cwd: nil, title: Heron.chatTitle))
            // Pinned: never auto-archived for being idle.
            app.setPinned(id, true)
            app.workspaceDidChange()
            open(id, deviceId: deviceId)
        } catch {
            status.show(.problem("Couldn't start the Heron chat: \(error.localizedDescription)"))
        }
    }

    private func open(_ id: String, deviceId: String) {
        guard let session = SessionViewController(app: app, chatId: id) else {
            // Not openable (yet): forget it and start over next time.
            Heron.setChatId(nil, on: deviceId)
            status.show(.problem("Couldn't open the Heron chat. Try again."))
            return
        }
        stopPolling()
        Heron.setChatId(id, on: deviceId)
        chatId = id
        UIView.transition(with: nav.view, duration: 0.25, options: .transitionCrossDissolve) {
            self.nav.setViewControllers([session], animated: false)
        }
    }

    /// The offline demo (screenshots, UI tests): its home chat stands in.
    private func openDemo() {
        guard let client = app.client else { return }
        let ws = client.workspace()
        let all = ws.front.recent + ws.projects.flatMap(\.sessions)
        guard let first = all.first(where: { $0.id == "chat-home" }) ?? all.first else {
            status.show(.working("Connecting…"))
            return keepPolling()
        }
        stopPolling()
        guard let session = SessionViewController(app: app, chatId: first.id) else { return }
        chatId = first.id
        nav.setViewControllers([session], animated: false)
    }

    /// Switch computers: the next resolve finds (or makes) Heron there.
    func switchComputer(to device: String) {
        guard device != Heron.deviceId else { return }
        Heron.deviceId = device
        chatId = nil
        nav.setViewControllers([status], animated: false)
        resolve()
    }

    // MARK: AppRouter (one chat: nothing else to route to)

    func openSession(_ chatId: String) {}
    func presentNewSession(prompt: String?) {}
    func showSettings() {}
    func showSearch() {}
}

extension UIViewController {
    var heronRoot: HeronRootController? { view.window?.rootViewController as? HeronRootController }
}

/// Connecting / setting up / pick a computer / a problem.
final class HeronStatusViewController: UIViewController, UITableViewDataSource, UITableViewDelegate {
    enum State: Equatable {
        case working(String)
        case pick([HeronWorld.Device])
        case problem(String)
    }

    var onPick: ((String) -> Void)?
    var onRetry: (() -> Void)?
    var onSignOut: (() -> Void)?
    var onChooseComputer: (() -> Void)?

    private let mark = UIImageView(image: UIImage(systemName: "bird.fill"))
    private let titleLabel = UILabel()
    private let message = UILabel()
    private let spinner = UIActivityIndicatorView(style: .medium)
    private let retry = UIButton(configuration: .bordered())
    private let choose = UIButton(configuration: .borderless())
    private let table = UITableView(frame: .zero, style: .insetGrouped)
    private var devices: [HeronWorld.Device] = []
    private var state: State?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        navigationItem.title = ""
        navigationItem.rightBarButtonItem = UIBarButtonItem(title: "Sign Out", primaryAction: UIAction { [weak self] _ in self?.onSignOut?() })
        mark.tintColor = Palette.text
        mark.contentMode = .scaleAspectFit
        mark.preferredSymbolConfiguration = .init(pointSize: 44, weight: .regular)
        titleLabel.text = "Heron"
        titleLabel.font = Fonts.ui(.sansSemibold, 28)
        titleLabel.textColor = Palette.text
        message.font = Fonts.ui(.sans, 16)
        message.textColor = Palette.secondary
        message.textAlignment = .center
        message.numberOfLines = 0
        message.accessibilityIdentifier = "heron-status"
        retry.configuration?.title = "Try Again"
        retry.addAction(UIAction { [weak self] _ in self?.onRetry?() }, for: .touchUpInside)
        choose.configuration?.title = "Choose Another Computer"
        choose.addAction(UIAction { [weak self] _ in self?.onChooseComputer?() }, for: .touchUpInside)
        let stack = UIStackView(arrangedSubviews: [mark, titleLabel, spinner, message, retry, choose])
        stack.axis = .vertical
        stack.alignment = .center
        stack.spacing = 12
        stack.setCustomSpacing(20, after: mark)
        stack.translatesAutoresizingMaskIntoConstraints = false
        table.dataSource = self
        table.delegate = self
        table.backgroundColor = .clear
        table.translatesAutoresizingMaskIntoConstraints = false
        table.register(UITableViewCell.self, forCellReuseIdentifier: "device")
        table.accessibilityIdentifier = "heron-computers"
        view.addSubview(stack)
        view.addSubview(table)
        let guide = view.safeAreaLayoutGuide
        NSLayoutConstraint.activate([
            stack.centerXAnchor.constraint(equalTo: guide.centerXAnchor),
            stack.topAnchor.constraint(equalTo: guide.topAnchor, constant: 80),
            stack.leadingAnchor.constraint(greaterThanOrEqualTo: guide.leadingAnchor, constant: 32),
            table.topAnchor.constraint(equalTo: stack.bottomAnchor, constant: 16),
            table.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            table.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            table.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        if let state { apply(state) }
    }

    func show(_ new: State) {
        guard new != state else { return }
        state = new
        if isViewLoaded { apply(new) }
    }

    private func apply(_ state: State) {
        switch state {
        case .working(let text):
            message.text = text
            spinner.startAnimating()
            spinner.isHidden = false
            retry.isHidden = true
            choose.isHidden = true
            devices = []
        case .pick(let list):
            message.text = "Which computer should Heron live on? Pick one that stays on: Heron works there, even in the background."
            spinner.stopAnimating()
            spinner.isHidden = true
            retry.isHidden = true
            choose.isHidden = true
            devices = list
        case .problem(let text):
            message.text = text
            spinner.stopAnimating()
            spinner.isHidden = true
            retry.isHidden = false
            choose.isHidden = false
            devices = []
        }
        table.isHidden = devices.isEmpty
        table.reloadData()
    }

    func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int { devices.count }

    func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = tableView.dequeueReusableCell(withIdentifier: "device", for: indexPath)
        let d = devices[indexPath.row]
        var content = cell.defaultContentConfiguration()
        content.text = d.name
        content.secondaryText = d.online ? "Online" : "Offline"
        content.image = UIImage(systemName: "laptopcomputer")
        cell.contentConfiguration = content
        cell.accessibilityIdentifier = "heron-computer-\(d.name)"
        return cell
    }

    func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        onPick?(devices[indexPath.row].id)
    }
}
