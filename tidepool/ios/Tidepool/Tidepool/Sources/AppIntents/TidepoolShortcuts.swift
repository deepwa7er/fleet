import AppIntents

struct TidepoolShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: SendToTidepoolIntent(),
            phrases: [
                "Send to \(.applicationName)",
                "Push to \(.applicationName)",
                "Add to \(.applicationName)",
            ],
            shortTitle: "Send to Tidepool",
            systemImageName: "arrow.up.circle.fill"
        )
    }
}
