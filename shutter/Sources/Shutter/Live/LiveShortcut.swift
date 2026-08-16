import Carbon.HIToolbox

// Live livestream shortcut — ⇧⌘9 (Shift+Command+9), not a symbolic hot key so no
// system takeover is needed. Mirrors ⌘⇧4 (region) with the same hand shape.

enum LiveShortcut {
    static let keyCode = UInt32(kVK_ANSI_9) // 25
    static let modifiers = UInt32(cmdKey | shiftKey)
    static let label = "⇧⌘9"
    static let hotKeyID: UInt32 = 0x4C495645 // 'LIVE'

    static var eventID: EventHotKeyID {
        EventHotKeyID(signature: 0x53485554, id: hotKeyID) // 'SHUT'
    }
}
