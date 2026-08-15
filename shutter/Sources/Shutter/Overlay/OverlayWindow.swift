import AppKit

/// A borderless, full-display window carrying the frozen screen and the
/// selection chrome drawn over it.
///
/// The frozen screenshot is an `NSImageView` underneath rather than something
/// `OverlayView` blits on every redraw: it is a native-resolution image of a
/// whole display, and repainting it on each mouse move to move a selection
/// rectangle by a pixel is work the window server can do once instead.
final class OverlayWindow: NSWindow {
    private let overlay: OverlayView

    init(display: FrozenDisplay, index: Int, controller: CaptureController) {
        overlay = OverlayView(display: display, index: index, controller: controller)
        super.init(contentRect: display.frame, styleMask: .borderless,
                   backing: .buffered, defer: false)

        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        // Above the menu bar, the Dock, and anybody's floating panel — the
        // overlay has to be able to select all of them.
        level = .screenSaver
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        acceptsMouseMovedEvents = true
        isReleasedWhenClosed = false

        let container = NSView(frame: CGRect(origin: .zero, size: display.frame.size))
        container.autoresizesSubviews = true

        let backdrop = NSImageView(frame: container.bounds)
        backdrop.image = NSImage(cgImage: display.image, size: display.frame.size)
        backdrop.imageScaling = .scaleAxesIndependently
        backdrop.autoresizingMask = [.width, .height]
        container.addSubview(backdrop)

        overlay.frame = container.bounds
        overlay.autoresizingMask = [.width, .height]
        container.addSubview(overlay)

        contentView = container
        setFrame(display.frame, display: false)
    }

    override var canBecomeKey: Bool { true }

    func refresh() { overlay.refresh() }
}
