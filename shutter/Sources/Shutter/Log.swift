import Foundation

/// Diagnostics go to stderr: the terminal picks them up when the binary is run
/// directly, and the system log picks them up when it is launched as a bundle.
enum Log {
    static func info(_ message: String) { write("Shutter: \(message)") }
    static func error(_ message: String) { write("Shutter: error: \(message)") }

    private static func write(_ line: String) {
        FileHandle.standardError.write(Data((line + "\n").utf8))
    }
}
