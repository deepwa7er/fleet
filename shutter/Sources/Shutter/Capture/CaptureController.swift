import AppKit
import Carbon.HIToolbox
import ScreenCaptureKit

enum OverlayMode {
    case region
    case window
    case display
}

/// Owns the selection UI: one full-screen overlay per display, the mode the
/// user is in, and the hit-testing that turns a drag or a click into a capture.
///
/// State lives here rather than in the views because there is one selection
/// across every display, and each view needs to render whichever part of it
/// falls on its own screen.
@MainActor
final class CaptureController {
    private(set) var mode: OverlayMode = .region
    private(set) var frozen: [FrozenDisplay] = []
    private(set) var hoveredWindow: PickableWindow?
    /// Index into `frozen`, valid in `.display` mode.
    private(set) var highlightedDisplay = 0
    /// Which display the cursor is over. Tracked separately from
    /// `highlightedDisplay` so that Tabbing to another display is not undone by
    /// the next twitch of the mouse on the one it is already sitting on.
    private var mouseDisplay = 0

    private var overlays: [OverlayWindow] = []
    private var pickable: [PickableWindow] = []
    private var keyMonitor: Any?
    private var isArmed = false

    private let onCapture: (CapturedImage) -> Void
    private let onFailure: (Error) -> Void

    init(onCapture: @escaping (CapturedImage) -> Void, onFailure: @escaping (Error) -> Void) {
        self.onCapture = onCapture
        self.onFailure = onFailure
    }

    // MARK: Lifecycle

    func arm() {
        guard !isArmed else { return }
        isArmed = true
        Task { @MainActor in
            do {
                let content = try await SCShareableContent.excludingDesktopWindows(
                    true, onScreenWindowsOnly: true)
                frozen = try await ScreenFreezer.freeze(content: content)
                pickable = WindowPicker.windows(from: content)
                present()
            } catch {
                isArmed = false
                onFailure(error)
            }
        }
    }

    /// ⌘⇧3's path: grab the display the pointer is on and go straight to the
    /// editor, with no selection UI in between.
    func captureDisplayUnderCursor() {
        guard !isArmed else { return }
        isArmed = true
        Task { @MainActor in
            do {
                let display = try await ScreenFreezer.freezeDisplayUnderCursor()
                isArmed = false
                onCapture(CapturedImage(image: display.image, scale: display.scale))
            } catch {
                isArmed = false
                onFailure(error)
            }
        }
    }

    private func present() {
        mode = .region
        hoveredWindow = nil
        let mouse = NSEvent.mouseLocation
        mouseDisplay = frozen.firstIndex { $0.frame.contains(mouse) } ?? 0
        highlightedDisplay = mouseDisplay

        for (index, display) in frozen.enumerated() {
            let overlay = OverlayWindow(display: display, index: index, controller: self)
            overlay.orderFrontRegardless()
            overlays.append(overlay)
        }
        // An accessory app gets no key events until it is frontmost, and the
        // overlay is useless without Esc.
        NSApp.activate(ignoringOtherApps: true)

        // A monitor rather than per-window key handling: with an overlay on
        // every display, the key window is whichever one the mouse happened to
        // land on, and Esc has to work regardless.
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            let consumed = MainActor.assumeIsolated { self?.handle(key: event) ?? false }
            return consumed ? nil : event
        }
    }

    /// Tears down the UI but leaves `isArmed` alone: window capture is async and
    /// must not let a second hot key press in while it is still in flight.
    private func dismissOverlays() {
        if let keyMonitor {
            NSEvent.removeMonitor(keyMonitor)
            self.keyMonitor = nil
        }
        overlays.forEach { $0.close() }
        overlays.removeAll()
        frozen.removeAll()
        pickable.removeAll()
        hoveredWindow = nil
    }

    func cancel() {
        dismissOverlays()
        isArmed = false
    }

    private func finish(with capture: CapturedImage) {
        dismissOverlays()
        isArmed = false
        onCapture(capture)
    }

    private func fail(_ error: Error) {
        dismissOverlays()
        isArmed = false
        onFailure(error)
    }

    // MARK: Keyboard

    /// Returns true when the key was consumed.
    private func handle(key event: NSEvent) -> Bool {
        switch Int(event.keyCode) {
        case kVK_Escape:
            cancel()
            return true
        case kVK_Space:
            mode = (mode == .window) ? .region : .window
            // Highlight whatever is already under the cursor rather than
            // waiting for the mouse to move before the picker looks alive.
            hoveredWindow = (mode == .window) ? window(at: NSEvent.mouseLocation) : nil
            refresh()
            return true
        case kVK_Tab:
            if mode == .display {
                highlightedDisplay = (highlightedDisplay + 1) % max(frozen.count, 1)
            } else {
                mode = .display
            }
            refresh()
            return true
        case kVK_Return, kVK_ANSI_KeypadEnter:
            guard mode == .display else { return false }
            captureDisplay(at: highlightedDisplay)
            return true
        default:
            return false
        }
    }

    private func refresh() {
        overlays.forEach { $0.refresh() }
    }

    // MARK: Hit testing, driven by the views

    func window(at point: CGPoint) -> PickableWindow? {
        pickable.first { $0.frame.contains(point) }
    }

    func mouseMoved(to point: CGPoint) {
        switch mode {
        case .window:
            let hit = window(at: point)
            guard hit?.scWindow.windowID != hoveredWindow?.scWindow.windowID else { return }
            hoveredWindow = hit
            refresh()
        case .display:
            guard let index = frozen.firstIndex(where: { $0.frame.contains(point) }),
                  index != mouseDisplay else { return }
            mouseDisplay = index
            highlightedDisplay = index
            refresh()
        case .region:
            // A region drag is confined to the display it started on, so the
            // view repaints itself and no other overlay needs to hear about it.
            break
        }
    }

    // MARK: Committing a capture

    func captureRegion(_ rect: CGRect, on display: FrozenDisplay) {
        guard let image = display.crop(appKitRect: rect) else {
            fail(ShutterError.regionOutsideDisplay)
            return
        }
        finish(with: CapturedImage(image: image, scale: display.scale))
    }

    func captureDisplay(at index: Int) {
        guard frozen.indices.contains(index) else {
            fail(ShutterError.noDisplays)
            return
        }
        let display = frozen[index]
        finish(with: CapturedImage(image: display.image, scale: display.scale))
    }

    func captureWindow(_ target: PickableWindow) {
        // Take the overlay down first so the window is unobscured and the app
        // is no longer holding a full-screen image per display while the
        // asynchronous capture runs.
        dismissOverlays()
        Task { @MainActor in
            do {
                let capture = try await WindowCapturer.capture(target.scWindow)
                isArmed = false
                onCapture(capture)
            } catch {
                isArmed = false
                onFailure(error)
            }
        }
    }
}
