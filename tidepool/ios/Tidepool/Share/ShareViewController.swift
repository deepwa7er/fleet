import UIKit
import UniformTypeIdentifiers
import os

final class ShareViewController: UIViewController {
    private static let log = Logger(subsystem: "com.deepwa7er.tidepool.share", category: "share")

    private let spinner = UIActivityIndicatorView(style: .large)
    private let label = UILabel()
    private var hasStarted = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        setupUI()
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        guard !hasStarted else { return }
        hasStarted = true
        Task { await pushAndDismiss() }
    }

    override var preferredContentSize: CGSize {
        get { CGSize(width: 280, height: 140) }
        set { super.preferredContentSize = newValue }
    }

    private func setupUI() {
        spinner.startAnimating()
        spinner.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(spinner)

        label.text = "Sending to tidepool…"
        label.textColor = .label
        label.font = .preferredFont(forTextStyle: .body)
        label.textAlignment = .center
        label.numberOfLines = 0
        label.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(label)

        NSLayoutConstraint.activate([
            spinner.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            spinner.centerYAnchor.constraint(equalTo: view.centerYAnchor, constant: -12),
            label.topAnchor.constraint(equalTo: spinner.bottomAnchor, constant: 12),
            label.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            label.leadingAnchor.constraint(greaterThanOrEqualTo: view.leadingAnchor, constant: 16),
            label.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -16),
        ])
    }

    private func pushAndDismiss() async {
        guard let text = await extractText(), !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            await MainActor.run {
                present(error: ShareError.nothingToShare)
            }
            return
        }

        do {
            try await push(text: text)
            await MainActor.run {
                extensionContext?.completeRequest(returningItems: nil, completionHandler: nil)
            }
        } catch {
            await MainActor.run {
                present(error: error)
            }
        }
    }

    private func push(text: String) async throws {
        let client = TidepoolClient(baseURL: Settings.serverURL())
        try await client.postClip(text: text)
        do {
            let updated = try await client.currentClip()
            try ClipCache().update(current: updated)
        } catch {
            Self.log.error("cache update after post failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    private func extractText() async -> String? {
        guard let item = extensionContext?.inputItems.first as? NSExtensionItem,
              let providers = item.attachments
        else { return nil }

        let textType = UTType.plainText.identifier
        let urlType = UTType.url.identifier

        if let provider = providers.first(where: { $0.hasItemConformingToTypeIdentifier(textType) }) {
            return await loadString(from: provider, typeID: textType)
        }
        if let provider = providers.first(where: { $0.hasItemConformingToTypeIdentifier(urlType) }) {
            return await loadString(from: provider, typeID: urlType)
        }
        return nil
    }

    private func loadString(from provider: NSItemProvider, typeID: String) async -> String? {
        await withCheckedContinuation { (continuation: CheckedContinuation<String?, Never>) in
            provider.loadItem(forTypeIdentifier: typeID, options: nil) { item, _ in
                let text = (item as? String) ?? (item as? URL)?.absoluteString
                continuation.resume(returning: text)
            }
        }
    }

    private func present(error: Error) {
        spinner.stopAnimating()
        label.text = nil

        let alert = UIAlertController(
            title: "Couldn't push to tidepool",
            message: error.localizedDescription,
            preferredStyle: .alert
        )
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel) { [weak self] _ in
            self?.extensionContext?.cancelRequest(withError: error)
        })
        alert.addAction(UIAlertAction(title: "Retry", style: .default) { [weak self] _ in
            guard let self else { return }
            self.spinner.startAnimating()
            self.label.text = "Sending to tidepool…"
            Task { await self.pushAndDismiss() }
        })
        present(alert, animated: true)
    }
}

private enum ShareError: LocalizedError {
    case nothingToShare

    var errorDescription: String? {
        switch self {
        case .nothingToShare:
            return "Nothing to share — couldn't find text or URL content."
        }
    }
}
