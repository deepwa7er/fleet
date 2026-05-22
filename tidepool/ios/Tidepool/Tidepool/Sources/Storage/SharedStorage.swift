import Foundation

enum AppGroup {
    static let identifier = "group.com.deepwa7er.tidepool"
}

enum SharedStorage {
    enum StorageError: Error, LocalizedError {
        case appGroupUnavailable

        var errorDescription: String? {
            switch self {
            case .appGroupUnavailable:
                return "App Group container is unavailable. Enable the App Group capability \"\(AppGroup.identifier)\" in the target's Signing & Capabilities, then re-run."
            }
        }
    }

    static func containerURL() throws -> URL {
        guard let url = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: AppGroup.identifier
        ) else {
            throw StorageError.appGroupUnavailable
        }
        return url
    }

    /// `UserDefaults(suiteName:)` returns a usable (but non-shared) instance
    /// even when the suite is not a configured App Group, so we verify via
    /// the container URL before vending defaults.
    static func defaults() throws -> UserDefaults {
        _ = try containerURL()
        guard let defaults = UserDefaults(suiteName: AppGroup.identifier) else {
            throw StorageError.appGroupUnavailable
        }
        return defaults
    }
}

enum Settings {
    static let defaultServerURL = URL(string: "https://tidepool.tailcfab97.ts.net")!
    private static let serverURLKey = "serverURL"

    static func serverURL() -> URL {
        guard let defaults = try? SharedStorage.defaults(),
              let raw = defaults.string(forKey: serverURLKey),
              let url = URL(string: raw)
        else {
            return defaultServerURL
        }
        return url
    }

    static func setServerURL(_ url: URL) throws {
        let defaults = try SharedStorage.defaults()
        defaults.set(url.absoluteString, forKey: serverURLKey)
    }
}
