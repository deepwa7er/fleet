import AppKit

// LiveRegionPicker — fullscreen crosshair overlay for livestream region selection.
// Reuses the DimView pattern from OverlayView but is video-live-specific: after
// the user drags a rect we start a live SCStream on that rect, not a still grab.
// For brevity we implement a single-display, single-rect picker (multi-display
// union like CaptureController is a follow-up).

final class LiveRegionPicker: NSWindow {
    private var onComplete: (CGRect?) -> Void
    private var startPoint: NSPoint = .zero
    private var currentRect: CGRect = .zero
    private var isDragging = false
    private let dimView = LiveDimView()
    private let chip = NSTextField(labelWithString: "")

    init(onComplete: @escaping (CGRect?) -> Void) {
        self.onComplete = onComplete
        let unionFrame = NSScreen.screens.reduce(NSRect.zero) { $0.union($1.frame) }
        super.init(contentRect: unionFrame, styleMask: .borderless, backing: .buffered, defer: false)
        level = .screenSaver
        backgroundColor = .clear
        isOpaque = false
        hasShadow = false
        isReleasedWhenClosed = false
        contentView = dimView
        dimView.frame = contentView!.bounds
        dimView.autoresizingMask = [.width, .height]

        chip.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
        chip.textColor = .white
        chip.backgroundColor = NSColor.black.withAlphaComponent(0.75)
        chip.isBezeled = false; chip.isEditable = false
        chip.wantsLayer = true; chip.layer?.cornerRadius = 4
        chip.alignment = .center
        chip.isHidden = true
        contentView?.addSubview(chip)
    }

    func show() {
        NSCursor.crosshair.push()
        makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { finish(nil) } // Esc
    }

    override func mouseDown(with event: NSEvent) {
        startPoint = event.locationInWindow
        isDragging = true
        currentRect = CGRect(origin: startPoint, size: .zero)
        dimView.selectionRect = currentRect
        chip.isHidden = false
        updateChip()
    }

    override func mouseDragged(with event: NSEvent) {
        guard isDragging else { return }
        let p = event.locationInWindow
        var r = CGRect(x: min(startPoint.x, p.x), y: min(startPoint.y, p.y),
                       width: abs(p.x - startPoint.x), height: abs(p.y - startPoint.y))
        if event.modifierFlags.contains(.shift) {
            let side = max(r.width, r.height)
            r.size = CGSize(width: side, height: side)
        }
        currentRect = r
        dimView.selectionRect = r
        updateChip()
    }

    override func mouseUp(with event: NSEvent) {
        guard isDragging else { return }
        isDragging = false
        if currentRect.width < 20 || currentRect.height < 20 { return }
        finish(currentRect)
    }

    private func updateChip() {
        chip.stringValue = "  \(Int(currentRect.width)) × \(Int(currentRect.height))  "
        chip.sizeToFit()
        var f = chip.frame
        f.origin = CGPoint(x: currentRect.midX - f.width/2, y: currentRect.maxY + 8)
        f.origin.x = max(8, min(f.origin.x, frame.width - f.width - 8))
        f.origin.y = max(8, min(f.origin.y, frame.height - f.height - 8))
        chip.frame = f
    }

    private func finish(_ rect: CGRect?) {
        NSCursor.pop()
        orderOut(nil)
        let r = rect
        DispatchQueue.main.async { [onComplete] in onComplete(r) }
    }
}

private final class LiveDimView: NSView {
    var selectionRect: CGRect = .zero { didSet { needsDisplay = true } }
    override func draw(_ dirtyRect: NSRect) {
        NSColor.black.withAlphaComponent(0.45).setFill()
        dirtyRect.fill()
        guard !selectionRect.isEmpty else { return }
        NSGraphicsContext.saveGraphicsState()
        let path = NSBezierPath(rect: bounds)
        path.append(NSBezierPath(rect: selectionRect))
        path.windingRule = .evenOdd
        NSColor.clear.setFill()
        path.fill()
        NSGraphicsContext.restoreGraphicsState()
        NSColor.white.withAlphaComponent(0.9).setStroke()
        let edge = NSBezierPath(rect: selectionRect.insetBy(dx: -0.5, dy: -0.5))
        edge.lineWidth = 1
        edge.stroke()
    }
}
