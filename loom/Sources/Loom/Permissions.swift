import ApplicationServices

enum Permissions {
    static var isTrusted: Bool { AXIsProcessTrusted() }

    /// Check Accessibility trust, optionally popping the system prompt.
    static func ensureAccessibility(prompt: Bool) -> Bool {
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue()
        return AXIsProcessTrustedWithOptions([key: prompt] as CFDictionary)
    }
}
