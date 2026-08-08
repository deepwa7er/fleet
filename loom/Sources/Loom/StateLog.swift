import Foundation

/// Tiny debug log readable from outside regardless of launch context — a
/// terminal child inherits the terminal's TCC grants, so only the
/// Finder-launched app's own report is trustworthy.
enum StateLog {
    private static let path = "/tmp/loom-state.txt"

    static func write(_ s: String) {
        try? (s + "\n").write(toFile: path, atomically: true, encoding: .utf8)
    }

    static func append(_ s: String) {
        let existing = (try? String(contentsOfFile: path, encoding: .utf8)) ?? ""
        try? (existing + s + "\n").write(toFile: path, atomically: true, encoding: .utf8)
    }
}
