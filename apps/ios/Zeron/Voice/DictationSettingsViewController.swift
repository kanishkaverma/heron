import Security
import UIKit

/// A separate, device-only Keychain item; never included in workspace sync.
enum GroqAPIKey {
    private static let service = "sh.zeron.ios.groq-dictation"
    private static let account = "api-key"
    static var value: String {
        Keychain.load(service: service, account: account).flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }
    static func save(_ value: String) throws {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
                                    kSecAttrService as String: service, kSecAttrAccount as String: account]
        if value.isEmpty {
            let status = SecItemDelete(query as CFDictionary)
            guard status == errSecSuccess || status == errSecItemNotFound else { throw StorageError() }
            return
        }
        let data = Data(value.utf8)
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var item = query
            item[kSecValueData as String] = data
            item[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            guard SecItemAdd(item as CFDictionary, nil) == errSecSuccess else { throw StorageError() }
        } else if status != errSecSuccess { throw StorageError() }
    }
    private struct StorageError: LocalizedError {
        var errorDescription: String? { "The API key could not be saved securely. Please unlock your device and retry." }
    }
}

final class DictationSettingsViewController: UIViewController {
    private let keyField = UITextField()
    private let status = UILabel()
    private let testButton = UIButton(type: .system)
    private let vocabulary = UITextView()
    private var task: Task<Void, Never>?

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Voice to Text"
        view.backgroundColor = Palette.background
        let intro = UILabel()
        intro.text = "Tap the mic in the composer to dictate. Your words land in the draft for you to edit; nothing is sent until you press Send.\n\nWithout a key, Heron transcribes on this iPhone (Apple’s on-device model, set up for Hindi and English). With a Groq key it’s more accurate: Groq’s Whisper Large v3 transcribes, then a text pass on Groq writes Hinglish in Roman script and uses your spellings below. If Groq can’t be reached, the iPhone’s model takes over."
        intro.numberOfLines = 0
        intro.textColor = Palette.text
        let privacy = UILabel()
        privacy.text = "Record, then ↑ to transcribe into your draft. ✕ cancels; the square pauses. With a key, audio goes straight from this iPhone to Groq only when you tap ↑, and Whisper’s text (not audio) goes to Groq’s gpt-oss model for the cleanup. The key stays in this iPhone’s Keychain, never in sync or on your computers. Groq usage charges may apply. Test Connection checks the key without sending audio."
        privacy.numberOfLines = 0
        privacy.font = .preferredFont(forTextStyle: .footnote)
        privacy.textColor = Palette.secondary
        keyField.placeholder = "Groq API key (gsk_…)"
        keyField.isSecureTextEntry = true
        keyField.autocorrectionType = .no
        keyField.autocapitalizationType = .none
        keyField.spellCheckingType = .no
        keyField.textContentType = .password
        keyField.borderStyle = .roundedRect
        keyField.text = GroqAPIKey.value
        keyField.accessibilityLabel = "Groq API key"
        keyField.accessibilityIdentifier = "dictation-api-key"
        keyField.addAction(UIAction { [weak self] _ in
            self?.task?.cancel()
            self?.status.text = "Unsaved changes"
            self?.testButton.isEnabled = true
        }, for: .editingChanged)
        let save = UIButton(type: .system)
        save.configuration = .filled()
        save.configuration?.title = "Save API Key"
        save.accessibilityIdentifier = "dictation-save-key"
        save.addAction(UIAction { [weak self] _ in self?.save() }, for: .touchUpInside)
        testButton.configuration = .bordered()
        testButton.configuration?.title = "Test Connection"
        testButton.accessibilityIdentifier = "dictation-test-key"
        testButton.addAction(UIAction { [weak self] _ in self?.test() }, for: .touchUpInside)
        status.numberOfLines = 0
        status.textColor = Palette.secondary
        status.accessibilityIdentifier = "dictation-key-status"
        let vocabularyLabel = UILabel()
        vocabularyLabel.text = "Names and words to get right"
        vocabularyLabel.font = .preferredFont(forTextStyle: .subheadline)
        vocabularyLabel.textColor = Palette.text
        vocabulary.text = DictationVocabulary.terms.joined(separator: ", ")
        vocabulary.font = .preferredFont(forTextStyle: .body)
        vocabulary.backgroundColor = .secondarySystemBackground
        vocabulary.layer.cornerRadius = 8
        vocabulary.autocorrectionType = .no
        vocabulary.autocapitalizationType = .none
        vocabulary.accessibilityLabel = "Names and words to get right, separated by commas"
        vocabulary.accessibilityIdentifier = "dictation-vocabulary"
        vocabulary.heightAnchor.constraint(equalToConstant: 96).isActive = true
        let stack = UIStackView(arrangedSubviews: [intro, keyField, save, testButton, status, vocabularyLabel, vocabulary, privacy])
        stack.axis = .vertical
        stack.spacing = 20
        stack.setCustomSpacing(8, after: vocabularyLabel)
        stack.translatesAutoresizingMaskIntoConstraints = false
        let scroll = UIScrollView()
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.keyboardDismissMode = .interactive
        view.addSubview(scroll)
        scroll.addSubview(stack)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 24),
            stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -24),
            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -24),
            stack.widthAnchor.constraint(equalTo: scroll.frameLayoutGuide.widthAnchor, constant: -48),
            keyField.heightAnchor.constraint(equalToConstant: 48),
        ])
    }

    private func save() {
        task?.cancel()
        testButton.isEnabled = true
        do {
            try GroqAPIKey.save(keyField.text ?? "")
            view.endEditing(true)
            status.text = GroqAPIKey.value.isEmpty ? "API key removed." : "API key saved securely on this device."
            status.textColor = Palette.secondary
        } catch { status.text = error.localizedDescription; status.textColor = Palette.danger }
    }

    private func test() {
        task?.cancel()
        let key = keyField.text ?? ""
        testButton.isEnabled = false
        status.textColor = Palette.secondary
        status.text = "Checking Groq…"
        task = Task { @MainActor [weak self] in
            do {
                try await GroqTranscription().testKey(key)
                guard !Task.isCancelled, let self else { return }
                self.status.text = "Connected — Whisper Large v3 is available."
                self.status.textColor = Palette.success
            } catch {
                guard !Task.isCancelled, let self else { return }
                self.status.text = error.localizedDescription
                self.status.textColor = Palette.danger
            }
            self?.testButton.isEnabled = true
        }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        DictationVocabulary.terms = DictationVocabulary.parse(vocabulary.text)
    }

    deinit { task?.cancel() }
}
