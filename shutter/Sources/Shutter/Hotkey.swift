import AppKit
import Carbon.HIToolbox

/// One of the screenshot shortcuts Shutter answers.
struct CaptureShortcut {
    let keyCode: UInt32
    let modifiers: UInt32
    let label: String
    /// The system symbolic hot key this displaces. The window server hands
    /// these to the screenshot service before any application hot key runs, so
    /// each one has to be switched off for its shortcut to reach Shutter.
    let symbolicHotKey: Int32
    let action: Action

    enum Action {
        /// Arm the overlay and let the user pick.
        case selection
        /// Grab the display under the cursor with no overlay at all.
        case display
    }
}

extension CaptureShortcut {
    /// ⌘⇧4 — where the system puts region selection, and where the muscle
    /// memory already is.
    static let region = CaptureShortcut(
        keyCode: UInt32(kVK_ANSI_4), modifiers: UInt32(cmdKey | shiftKey),
        label: "⌘⇧4", symbolicHotKey: 30, action: .selection)

    /// ⌘⇧3 — the system writes the whole screen straight to a file. Shutter
    /// grabs the display under the cursor and opens it for markup instead.
    static let wholeDisplay = CaptureShortcut(
        keyCode: UInt32(kVK_ANSI_3), modifiers: UInt32(cmdKey | shiftKey),
        label: "⌘⇧3", symbolicHotKey: 28, action: .display)

    /// ⌘⇧5 — the system shows a toolbar of capture modes. The overlay's own
    /// mode switching is the equivalent, so it lands in the same place.
    static let options = CaptureShortcut(
        keyCode: UInt32(kVK_ANSI_5), modifiers: UInt32(cmdKey | shiftKey),
        label: "⌘⇧5", symbolicHotKey: 184, action: .selection)

    static let all = [region, wholeDisplay, options]
}

/// Global hot keys registered with the Carbon hot-key API.
///
/// Carbon hot keys are used rather than a `CGEventTap` because they need no
/// Accessibility grant — Shutter already asks for Screen Recording, and one
/// permission prompt is enough — and because the system cannot silently
/// disable them the way it disables an unresponsive tap.
final class HotkeyManager {
    private var handlerRef: EventHandlerRef?
    private var registered: [UInt32: (ref: EventHotKeyRef, shortcut: CaptureShortcut)] = [:]
    private var nextID: UInt32 = 1
    private let onTrigger: (CaptureShortcut) -> Void

    /// Four-char signature identifying Shutter's hot keys ("SHUT").
    private static let signature: OSType = 0x53485554

    private(set) var failed: [CaptureShortcut] = []

    init(onTrigger: @escaping (CaptureShortcut) -> Void) {
        self.onTrigger = onTrigger
        installHandler()
    }

    deinit {
        registered.values.forEach { UnregisterEventHotKey($0.ref) }
        if let handlerRef { RemoveEventHandler(handlerRef) }
    }

    func register(_ shortcuts: [CaptureShortcut]) {
        for shortcut in shortcuts {
            var ref: EventHotKeyRef?
            let id = EventHotKeyID(signature: Self.signature, id: nextID)
            let status = RegisterEventHotKey(shortcut.keyCode, shortcut.modifiers, id,
                                             GetApplicationEventTarget(), 0, &ref)
            // Note that a `noErr` here is not proof the key will ever fire: the
            // system's own symbolic hot keys win silently. That is what
            // `SystemScreenshotShortcuts` exists to deal with.
            if status == noErr, let ref {
                registered[nextID] = (ref, shortcut)
                nextID += 1
            } else {
                failed.append(shortcut)
                Log.error("could not register \(shortcut.label) (status \(status)); "
                          + "another app has probably already claimed it")
            }
        }
    }

    private func installHandler() {
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard),
                                 eventKind: UInt32(kEventHotKeyPressed))
        let handler: EventHandlerUPP = { _, event, userData in
            guard let event, let userData else { return noErr }
            let manager = Unmanaged<HotkeyManager>.fromOpaque(userData).takeUnretainedValue()
            var id = EventHotKeyID()
            GetEventParameter(event, EventParamName(kEventParamDirectObject),
                              EventParamType(typeEventHotKeyID), nil,
                              MemoryLayout<EventHotKeyID>.size, nil, &id)
            guard id.signature == HotkeyManager.signature else { return noErr }
            manager.dispatch(id.id)
            return noErr
        }
        InstallEventHandler(GetApplicationEventTarget(), handler, 1, &spec,
                            Unmanaged.passUnretained(self).toOpaque(), &handlerRef)
    }

    private func dispatch(_ id: UInt32) {
        guard let entry = registered[id] else { return }
        onTrigger(entry.shortcut)
    }
}
