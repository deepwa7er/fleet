import AppIntents
import Foundation
import os

struct SendToTidepoolIntent: AppIntent {
    static let title: LocalizedStringResource = "Send to tidepool"
    static let description = IntentDescription(
        "Push a clip to your tidepool tailnet so it shows up on every device.",
        categoryName: "Clipboard"
    )

    static let openAppWhenRun: Bool = false

    @Parameter(
        title: "Content",
        description: "The text or URL to push to tidepool.",
        inputOptions: .init(
            keyboardType: .default,
            capitalizationType: .none,
            multiline: true,
            autocorrect: false,
            smartQuotes: false,
            smartDashes: false
        )
    )
    var content: String

    static var parameterSummary: some ParameterSummary {
        Summary("Send \(\.$content) to tidepool")
    }

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let log = Logger(subsystem: "com.deepwa7er.Tidepool", category: "intent")
        let trimmed = content.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw IntentError.empty
        }

        let serverURL = await Settings.serverURL()
        let client = TidepoolClient(baseURL: serverURL)
        try await client.postClip(text: content)

        do {
            let updated = try await client.currentClip()
            try await ClipCache().update(current: updated)
        } catch {
            log.error("cache update after intent post failed: \(error.localizedDescription, privacy: .public)")
        }

        return .result(dialog: "Sent to tidepool.")
    }
}

enum IntentError: Error, CustomLocalizedStringResourceConvertible {
    case empty

    var localizedStringResource: LocalizedStringResource {
        switch self {
        case .empty:
            return "Nothing to send — the content was empty."
        }
    }
}
