import Foundation

/// Persisted ⌘-digit → app mapping, so an arrangement like "RustRover is
/// ⌘1" survives restarts. Keyed by digit: each names the bundle ID it
/// belongs to; an app with several windows can hold several digits. Digits
/// reserved for apps that aren't currently running are left untouched, so
/// quitting an app doesn't forfeit its number.
enum Assignments {
    private static let key = "windowNumbers"

    static func load() -> [Int: String] {
        guard let raw = UserDefaults.standard.dictionary(forKey: key) as? [String: String]
        else { return [:] }
        var map: [Int: String] = [:]
        for (number, appID) in raw {
            if let n = Int(number) { map[n] = appID }
        }
        return map
    }

    static func save(_ map: [Int: String]) {
        let raw = Dictionary(uniqueKeysWithValues: map.map { (String($0.key), $0.value) })
        UserDefaults.standard.set(raw, forKey: key)
    }
}
