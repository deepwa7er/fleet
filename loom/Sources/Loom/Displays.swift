import AppKit

/// Which display the stack lives on: enumeration, persisted selection, and
/// screen geometry in Quartz (top-left origin) coordinates to match AX and
/// CGWindowList. Selection is stored by display UUID, which survives
/// reconnects and display-ID churn; when the selected display isn't
/// connected, the stack falls back to the primary display.
enum Displays {
    private static let defaultsKey = "selectedDisplayUUID"

    /// Stable identity for a screen across reconnects.
    static func uuid(of screen: NSScreen) -> String? {
        guard let number = screen.deviceDescription[
                  NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber,
              let cfUUID = CGDisplayCreateUUIDFromDisplayID(number.uint32Value)?
                  .takeRetainedValue()
        else { return nil }
        return CFUUIDCreateString(nil, cfUUID) as String
    }

    /// The persisted selection; nil means "the primary display". Read once at
    /// startup by the stack — the live selection is stack state, so the stage
    /// can never silently jump displays because a preference changed out from
    /// under a running session.
    static func savedSelection() -> String? {
        UserDefaults.standard.string(forKey: defaultsKey)
    }

    static func persistSelection(_ uuid: String?) {
        UserDefaults.standard.set(uuid, forKey: defaultsKey)
    }

    /// Resolve a selection to a connected screen, falling back to the
    /// primary display when the selection is nil or disconnected.
    static func screen(matching uuid: String?) -> NSScreen? {
        let screens = NSScreen.screens
        guard let uuid,
              let match = screens.first(where: { self.uuid(of: $0) == uuid })
        else { return screens.first }
        return match
    }

    /// Full display frame in Quartz coordinates.
    static func frame(of screen: NSScreen) -> CGRect {
        quartzRect(screen.frame)
    }

    /// Display frame minus menu bar and Dock, in Quartz coordinates.
    static func visibleFrame(of screen: NSScreen) -> CGRect {
        quartzRect(screen.visibleFrame)
    }

    /// Cocoa global rects have a bottom-left origin; Quartz has a top-left
    /// origin. Both are anchored to the primary display, so the flip uses
    /// the primary's height — using each screen's own height (as this code
    /// once did) is only coincidentally correct on the primary display.
    private static func quartzRect(_ rect: CGRect) -> CGRect {
        guard let primary = NSScreen.screens.first else { return rect }
        return CGRect(x: rect.minX, y: primary.frame.maxY - rect.maxY,
                      width: rect.width, height: rect.height)
    }
}
