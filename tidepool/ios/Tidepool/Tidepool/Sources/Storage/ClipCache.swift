import Foundation

struct CachedState: Codable, Equatable {
    var current: Clip?
    var recent: [Clip]

    static let empty = CachedState(current: nil, recent: [])
}

/// File-backed cache shared across app targets via the App Group container.
struct ClipCache {
    static let maxRecent = 50

    private let url: URL

    init() throws {
        let container = try SharedStorage.containerURL()
        self.url = container.appending(path: "cache.json", directoryHint: .notDirectory)
    }

    func read() throws -> CachedState {
        guard FileManager.default.fileExists(atPath: url.path) else {
            return .empty
        }
        let data = try Data(contentsOf: url)
        if data.isEmpty { return .empty }
        return try JSON.decoder.decode(CachedState.self, from: data)
    }

    func update(current: Clip) throws {
        var state = (try? read()) ?? .empty
        state.current = current
        if state.recent.first != current {
            state.recent.insert(current, at: 0)
            if state.recent.count > Self.maxRecent {
                state.recent.removeLast(state.recent.count - Self.maxRecent)
            }
        }
        try write(state)
    }

    /// `Data.write(to:options:.atomic)` writes via a temp file and renames,
    /// so the cache is never observed half-written. Cross-target concurrent
    /// writes (e.g. share extension + main app) can still clobber each
    /// other; that's acceptable for target #1 and will be revisited when a
    /// second writer is introduced.
    private func write(_ state: CachedState) throws {
        let data = try JSON.encoder.encode(state)
        try data.write(to: url, options: [.atomic])
    }
}
