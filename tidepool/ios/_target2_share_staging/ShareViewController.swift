import UIKit
import Social
import UniformTypeIdentifiers
import os

final class ShareViewController: SLComposeServiceViewController {
    private static let log = Logger(subsystem: "com.deepwa7er.tidepool.share", category: "share")

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Tidepool"
        placeholder = "Push to tidepool"
        loadInitialContent()
    }

    override func isContentValid() -> Bool {
        let text = contentText ?? ""
        return !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    override func didSelectPost() {
        pushAndDismiss()
    }

    override func configurationItems() -> [Any]! {
        return []
    }

    private func pushAndDismiss() {
        let text = contentText ?? ""
        Task { @MainActor in
            do {
                try await push(text: text)
                self.extensionContext?.completeRequest(returningItems: nil, completionHandler: nil)
            } catch {
                self.presentError(error)
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

    private func presentError(_ error: Error) {
        let alert = UIAlertController(
            title: "Couldn't push to tidepool",
            message: error.localizedDescription,
            preferredStyle: .alert
        )
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel) { [weak self] _ in
            self?.extensionContext?.cancelRequest(withError: error)
        })
        alert.addAction(UIAlertAction(title: "Retry", style: .default) { [weak self] _ in
            self?.pushAndDismiss()
        })
        present(alert, animated: true)
    }

    private func loadInitialContent() {
        guard let item = extensionContext?.inputItems.first as? NSExtensionItem,
              let providers = item.attachments
        else { return }

        let textType = UTType.plainText.identifier
        let urlType = UTType.url.identifier

        if let provider = providers.first(where: { $0.hasItemConformingToTypeIdentifier(textType) }) {
            provider.loadItem(forTypeIdentifier: textType, options: nil) { [weak self] item, _ in
                let text = (item as? String) ?? (item as? URL)?.absoluteString
                DispatchQueue.main.async { self?.applyInitialText(text) }
            }
            return
        }

        if let provider = providers.first(where: { $0.hasItemConformingToTypeIdentifier(urlType) }) {
            provider.loadItem(forTypeIdentifier: urlType, options: nil) { [weak self] item, _ in
                let text = (item as? URL)?.absoluteString ?? (item as? String)
                DispatchQueue.main.async { self?.applyInitialText(text) }
            }
            return
        }
    }

    private func applyInitialText(_ text: String?) {
        guard let text, !text.isEmpty else { return }
        textView.text = text
        validateContent()
    }
}
