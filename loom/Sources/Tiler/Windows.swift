import AppKit
import ApplicationServices

/// A window enrolled in the stack: CG identity, AX handle, and the frame to
/// give back on restore. A class (not struct) so the write cache and the
/// assigned digit live on it.
final class ManagedWindow {
    let id: CGWindowID
    let pid: pid_t
    let axWindow: AXUIElement
    let originalFrame: CGRect
    /// Owning app's bundle identifier; the key for persisted ⌘-digits.
    let appID: String?
    /// The last frame we wrote, so repeat writes of the same geometry are
    /// skipped. Only tracks our own writes — a window the user drags out of the
    /// tile is left alone, which is deliberate.
    var lastFrame: CGRect?
    /// ⌘-digit assigned to this window (1–9), if any. Session-scoped.
    var number: Int?

    init(id: CGWindowID, pid: pid_t, originalFrame: CGRect, axWindow: AXUIElement,
         appID: String?) {
        self.id = id
        self.pid = pid
        self.originalFrame = originalFrame
        self.axWindow = axWindow
        self.appID = appID
    }
}

enum Windows {
    /// A CG-reported on-screen window, before AX enrollment.
    struct Info {
        let id: CGWindowID
        let pid: pid_t
        let bounds: CGRect
    }

    /// Owners whose layer-0 "windows" are never useful stack members.
    private static let ignoredOwners: Set<String> = [
        "Window Server", "Dock", "Control Center", "WindowManager", "Notification Center",
    ]

    /// Every on-screen window of a regular app whose center lies on the
    /// given display (Quartz frame), front-to-back.
    static func snapshot(on display: CGRect) -> [Info] {
        let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
        guard let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]]
        else { return [] }
        let ownPID = ProcessInfo.processInfo.processIdentifier
        // The kCGWindow* dictionary keys aren't exposed to Swift; use literals.
        return list.compactMap { info in
            guard let pid = info["kCGWindowOwnerPID"] as? pid_t,
                  pid != ownPID,
                  let id = info["kCGWindowNumber"] as? CGWindowID,
                  info["kCGWindowLayer"] as? Int == 0,
                  let owner = info["kCGWindowOwnerName"] as? String,
                  !ignoredOwners.contains(owner),
                  let boundsDict = info["kCGWindowBounds"] as? [String: Any],
                  let bounds = CGRect(dictionaryRepresentation: boundsDict as CFDictionary),
                  display.contains(CGPoint(x: bounds.midX, y: bounds.midY))
            else { return nil }
            return Info(id: id, pid: pid, bounds: bounds)
        }
    }

    /// Enroll a CG window: resolve its AX window and verify we can move it.
    static func manage(_ info: Info) -> ManagedWindow? {
        guard let ax = findAXWindow(matching: info),
              isSettable(ax, kAXPositionAttribute),
              isSettable(ax, kAXSizeAttribute),
              !isFullScreen(ax)
        else { return nil }
        return ManagedWindow(id: info.id, pid: info.pid, originalFrame: info.bounds,
                             axWindow: ax,
                             appID: NSRunningApplication(processIdentifier: info.pid)?
                                 .bundleIdentifier)
    }

    static func isAlive(_ ax: AXUIElement) -> Bool {
        var value: AnyObject?
        return AXUIElementCopyAttributeValue(ax, kAXRoleAttribute as CFString, &value) == .success
    }

    // MARK: Mutation

    /// Move and resize, skipping no-op frames. Used at enrollment (to apply the
    /// tile), when the display geometry changes, and once per window on restore.
    static func setFrame(_ window: ManagedWindow, _ rect: CGRect) {
        if let last = window.lastFrame,
           abs(last.minX - rect.minX) < 0.5, abs(last.minY - rect.minY) < 0.5,
           abs(last.width - rect.width) < 0.5, abs(last.height - rect.height) < 0.5 { return }
        setFrame(window.axWindow, rect)
        window.lastFrame = rect
    }

    /// Write the frame whether or not it matches what we last asked for.
    ///
    /// `setFrame(_:_:)` skips the write when the rect matches `lastFrame`, which
    /// keeps the reconcile pass cheap. But `lastFrame` records what Tiler
    /// *requested*, not where the window actually is — a window the user has
    /// since dragged or resized still matches it. Retiling would then skip
    /// precisely the windows that have drifted, which are the only ones that
    /// needed moving.
    static func forceFrame(_ window: ManagedWindow, _ rect: CGRect) {
        setFrame(window.axWindow, rect)
        window.lastFrame = rect
    }

    /// Apps clamp a new position against the *current* size and vice versa, so
    /// position → size → position converges regardless of starting geometry.
    static func setFrame(_ ax: AXUIElement, _ rect: CGRect) {
        setPoint(ax, kAXPositionAttribute, rect.origin)
        setSize(ax, kAXSizeAttribute, rect.size)
        setPoint(ax, kAXPositionAttribute, rect.origin)
    }

    /// The frontmost of `ids` in the WindowServer's current z-order — which of
    /// our windows the user is actually looking at — or nil when none of them
    /// are on screen.
    static func frontmost(among ids: Set<CGWindowID>) -> CGWindowID? {
        let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
        guard let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]]
        else { return nil }
        // The list is ordered front-to-back.
        for info in list {
            guard let wid = info["kCGWindowNumber"] as? CGWindowID, ids.contains(wid)
            else { continue }
            return wid
        }
        return nil
    }

    static func raise(_ ax: AXUIElement) {
        AXUIElementPerformAction(ax, kAXRaiseAction as CFString)
    }

    /// Human-readable label for menus: the window's AX title, else its app's
    /// name, else the bare window number.
    static func title(of window: ManagedWindow) -> String {
        var value: AnyObject?
        if AXUIElementCopyAttributeValue(window.axWindow, kAXTitleAttribute as CFString,
                                         &value) == .success,
           let title = value as? String, !title.isEmpty {
            return title
        }
        return NSRunningApplication(processIdentifier: window.pid)?.localizedName
            ?? "Window \(window.id)"
    }

    /// Keyboard focus + bring the app forward, without disturbing its other
    /// windows.
    static func focus(_ window: ManagedWindow) {
        AXUIElementSetAttributeValue(window.axWindow, kAXMainAttribute as CFString, kCFBooleanTrue)
        AXUIElementSetAttributeValue(window.axWindow, kAXFocusedAttribute as CFString, kCFBooleanTrue)
        raise(window.axWindow)
        NSRunningApplication(processIdentifier: window.pid)?.activate(options: [])
    }

    // MARK: AX plumbing

    /// CG and AX window lists share no identifier, so match on geometry: an
    /// exact frame match is unambiguous in practice, unlike titles.
    private static func findAXWindow(matching info: Info) -> AXUIElement? {
        let app = AXUIElementCreateApplication(info.pid)
        var value: AnyObject?
        guard AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &value) == .success,
              let axWindows = value as? [AXUIElement]
        else { return nil }
        return axWindows.first { ax in
            guard let (pos, size) = geometry(of: ax) else { return false }
            return abs(pos.x - info.bounds.minX) < 1
                && abs(pos.y - info.bounds.minY) < 1
                && abs(size.width - info.bounds.width) < 1
                && abs(size.height - info.bounds.height) < 1
        }
    }

    private static func isSettable(_ ax: AXUIElement, _ attribute: String) -> Bool {
        var settable = DarwinBoolean(false)
        return AXUIElementIsAttributeSettable(ax, attribute as CFString, &settable) == .success
            && settable.boolValue
    }

    private static func isFullScreen(_ ax: AXUIElement) -> Bool {
        var value: AnyObject?
        // Queried by literal name; the constant isn't exposed to Swift.
        guard AXUIElementCopyAttributeValue(ax, "AXFullScreen" as CFString, &value) == .success
        else { return false }
        return (value as? Bool) ?? false
    }

    private static func geometry(of ax: AXUIElement) -> (CGPoint, CGSize)? {
        var posRef: AnyObject?
        var sizeRef: AnyObject?
        guard AXUIElementCopyAttributeValue(ax, kAXPositionAttribute as CFString, &posRef) == .success,
              AXUIElementCopyAttributeValue(ax, kAXSizeAttribute as CFString, &sizeRef) == .success,
              let posRef, let sizeRef,
              CFGetTypeID(posRef) == AXValueGetTypeID(),
              CFGetTypeID(sizeRef) == AXValueGetTypeID()
        else { return nil }
        var pos = CGPoint.zero
        var size = CGSize.zero
        AXValueGetValue((posRef as! AXValue), .cgPoint, &pos)
        AXValueGetValue((sizeRef as! AXValue), .cgSize, &size)
        return (pos, size)
    }

    private static func setPoint(_ ax: AXUIElement, _ attribute: String, _ point: CGPoint) {
        var p = point
        if let value = AXValueCreate(.cgPoint, &p) {
            AXUIElementSetAttributeValue(ax, attribute as CFString, value)
        }
    }

    private static func setSize(_ ax: AXUIElement, _ attribute: String, _ size: CGSize) {
        var s = size
        if let value = AXValueCreate(.cgSize, &s) {
            AXUIElementSetAttributeValue(ax, attribute as CFString, value)
        }
    }
}
