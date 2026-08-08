import AppKit
import ScreenCaptureKit

/// A window the picker can highlight and capture.
struct PickableWindow {
    let scWindow: SCWindow
    /// AppKit global frame, in points, for drawing the highlight.
    let frame: CGRect
    let label: String
}

enum WindowPicker {
    /// On-screen application windows, front to back.
    ///
    /// The z-order comes from `CGWindowListCopyWindowInfo`, which documents
    /// that it returns windows front to back. `SCShareableContent` promises no
    /// ordering at all, and hit-testing overlapping windows without a real
    /// z-order picks the wrong one exactly when it matters — when two windows
    /// overlap under the cursor.
    static func windows(from content: SCShareableContent) -> [PickableWindow] {
        let byID = Dictionary(content.windows.map { ($0.windowID, $0) },
                              uniquingKeysWith: { first, _ in first })
        let ownPID = ProcessInfo.processInfo.processIdentifier
        let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements],
                                              kCGNullWindowID) as? [[String: Any]] ?? []

        return info.compactMap { entry -> PickableWindow? in
            // Layer 0 is the normal application window layer. Anything else is
            // a panel, a menu, the Dock, or someone's floating HUD — none of
            // which anyone means to screenshot by clicking on it.
            guard let layer = entry[kCGWindowLayer as String] as? Int, layer == 0,
                  let pid = entry[kCGWindowOwnerPID as String] as? pid_t, pid != ownPID,
                  let id = entry[kCGWindowNumber as String] as? CGWindowID,
                  let window = byID[id], window.isOnScreen
            else { return nil }

            // SCWindow.frame is authoritative and already in CoreGraphics
            // global points; CGWindowList is consulted only for the ordering.
            let frame = Geometry.appKitRect(fromCG: window.frame)
            guard frame.width >= 1, frame.height >= 1 else { return nil }

            let owner = window.owningApplication?.applicationName ?? ""
            let title = window.title ?? ""
            let label = [owner, title].filter { !$0.isEmpty }.joined(separator: " — ")
            return PickableWindow(scWindow: window, frame: frame, label: label)
        }
    }
}
