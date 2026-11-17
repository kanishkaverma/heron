import UIKit

/// The signed-in app: the one Heron chat, full screen. It isn't tied to a
/// computer: heron-keeper on each computer makes the chat, keeps it on one
/// that's online and moves it when that one goes down; this screen just finds
/// it and follows it. Until it exists, a status page says what's happening.
final class HeronRootController: UIViewController, AppRouter {
    private let app: AppModel
    private let nav = UINavigationController()
    private let status = HeronStatusViewController()
    private var appToken: AnyObject?
    private var poll: Timer?
    private(set) var chatId: String?

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
        status.onRetry = { [weak self] in self?.resolve() }
        status.onSignOut = { [weak self] in self?.app.signOut() }
        appToken = app.observe { [weak self] in self?.resolveIfWaiting() }
        resolve()
    }

    override var childForStatusBarStyle: UIViewController? { nav }

    private func world() -> HeronWorld? {
        guard let client = app.client else { return nil }
        let ws = client.workspace()
        let devices = client.executionDevices().map { HeronWorld.Device(id: $0.id, name: $0.name, online: $0.online) }
        var seen = Set<String>()
        var chats: [HeronWorld.Chat] = []
        func add(_ r: SessionRow) {
            guard seen.insert(r.id).inserted else { return }
            chats.append(HeronWorld.Chat(id: r.id, title: r.title, cwd: r.cwd, deviceId: r.deviceId, parentId: r.parentChatId, archived: r.archived, createdAtMs: r.createdAtMs))
        }
        ws.front.recent.forEach(add)
        ws.front.pinned.forEach(add)
        ws.front.sections.forEach { $0.sessions.forEach(add) }
        ws.projectless.forEach(add)
        ws.projects.forEach { $0.sessions.forEach(add) }
        ws.archived.forEach(add)
        return HeronWorld(synced: ws.synced, devices: devices, chats: chats)
    }

    private func resolveIfWaiting() {
        guard chatId == nil else { return }
        resolve()
    }

    func resolve() {
        guard chatId == nil else { return }
        if app.isDemo { return openDemo() }
        guard let world = world() else { return }
        switch HeronStep.next(world, savedChat: Heron.chatId) {
        case .waitForSync:
            status.show(.working("Connecting…"))
            keepPolling()
        case .waitForChat(let online):
            status.show(online
                ? .working("Starting Heron…")
                : .problem("None of your computers is online. Heron runs on air, zeph or your MacBook: wake one up and it'll pick up."))
            keepPolling()
        case let .open(chatId, _, unarchive):
            if unarchive { app.unarchive(chatId) }
            open(chatId)
        }
    }

    private func keepPolling() {
        guard poll == nil else { return }
        poll = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.resolveIfWaiting() }
        }
    }

    private func stopPolling() {
        poll?.invalidate()
        poll = nil
    }

    private func open(_ id: String) {
        guard let session = SessionViewController(app: app, chatId: id) else {
            Heron.chatId = nil
            status.show(.problem("Couldn't open the Heron chat. Try again."))
            return
        }
        stopPolling()
        Heron.chatId = id
        chatId = id
        // Pinned: never auto-archived for being idle.
        if app.session(id)?.pinned == false { app.setPinned(id, true) }
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

    private let mark = UIImageView(image: UIImage(systemName: "bird.fill"))
    private let titleLabel = UILabel()
    private let message = UILabel()
    private let spinner = UIActivityIndicatorView(style: .medium)
    private let retry = UIButton(configuration: .bordered())
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
        let stack = UIStackView(arrangedSubviews: [mark, titleLabel, spinner, message, retry])
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
            devices = []
        case .pick(let list):
            message.text = "Which computer should Heron live on? Pick one that stays on: Heron works there, even in the background."
            spinner.stopAnimating()
            spinner.isHidden = true
            retry.isHidden = true
            devices = list
        case .problem(let text):
            message.text = text
            spinner.stopAnimating()
            spinner.isHidden = true
            retry.isHidden = false
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
