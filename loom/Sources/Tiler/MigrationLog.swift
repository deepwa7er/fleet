import Foundation
import os

/// Where the chip row migration reports what it actually did.
///
/// Both implementations render identical chips — that is the goal of a parity
/// migration, and it is also why there is nothing on screen to tell them apart.
/// The only evidence that the reconciler is live, and that it is doing less
/// work than the code it replaces, is this.
///
///     tail -f ~/Library/Logs/Tiler/filament.log
///
/// Also goes to the unified log, for anyone who prefers `log stream`:
///
///     log stream --predicate 'subsystem == "net.deepwa7er.tiler"' --level info
enum MigrationLog {
    private static let logger = Logger(subsystem: "net.deepwa7er.tiler", category: "migration")

    static let fileURL: URL = {
        let directory = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Logs/Tiler", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.appending(path: "filament.log")
    }()

    private static let clock: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss.SSS"
        return formatter
    }()

    static func note(_ message: String) {
        logger.notice("\(message, privacy: .public)")
        append("\(clock.string(from: Date()))  \(message)\n")
    }

    /// Marks a new run, so a tailed file never blurs two launches together.
    static func startSession() {
        let stamp = ISO8601DateFormatter().string(from: Date())
        let path = FeatureFlags.filamentChips ? "Filament reconciler" : "legacy rebuild"
        append("\n=== Tiler launched \(stamp) — chip rows: \(path) ===\n")
    }

    /// Opened and closed per line rather than held. Slower, but this logs at
    /// the rate a person clicks things, and a handle held across the app's life
    /// is a handle to lose track of.
    private static func append(_ text: String) {
        guard let data = text.data(using: .utf8) else { return }
        if let handle = try? FileHandle(forWritingTo: fileURL) {
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: data)
        } else {
            try? data.write(to: fileURL)
        }
    }
}
