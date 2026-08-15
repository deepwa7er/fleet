import AppKit

/// The selection chrome for one display: the dimming, the current selection,
/// the magnifier, and the mode hint.
///
/// It draws no screen content of its own — the frozen screenshot sits in a view
/// underneath, and the undimmed selection is a hole punched through the dim.
final class OverlayView: NSView {
    private let display: FrozenDisplay
    private let index: Int
    private weak var controller: CaptureController?

    /// The in-progress region drag, in view coordinates.
    private var draft: CGRect?
    private var dragOrigin: CGPoint?
    /// Where the cursor is on this display, or nil when it is on another one.
    private var mouse: CGPoint?

    private let dimAlpha: CGFloat = 0.45
    private let loupeSide: CGFloat = 128
    private let loupeZoom: CGFloat = 8
    private let minimumSelection: CGFloat = 3

    init(display: FrozenDisplay, index: Int, controller: CaptureController) {
        self.display = display
        self.index = index
        self.controller = controller
        super.init(frame: .zero)
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func refresh() {
        needsDisplay = true
        window?.invalidateCursorRects(for: self)
    }

    // MARK: Coordinates

    /// View point → AppKit global point.
    private func global(_ point: CGPoint) -> CGPoint {
        CGPoint(x: point.x + display.frame.minX, y: point.y + display.frame.minY)
    }

    /// AppKit global rect → view rect.
    private func local(_ rect: CGRect) -> CGRect {
        rect.offsetBy(dx: -display.frame.minX, dy: -display.frame.minY)
    }

    /// View rect → AppKit global rect.
    private func global(_ rect: CGRect) -> CGRect {
        rect.offsetBy(dx: display.frame.minX, dy: display.frame.minY)
    }

    private func point(for event: NSEvent) -> CGPoint {
        convert(event.locationInWindow, from: nil)
    }

    // MARK: Event plumbing

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(
            rect: .zero,
            options: [.mouseEnteredAndExited, .mouseMoved, .activeAlways, .inVisibleRect],
            owner: self))
    }

    override func resetCursorRects() {
        let cursor: NSCursor = (controller?.mode == .region) ? .crosshair : .arrow
        addCursorRect(bounds, cursor: cursor)
    }

    override func mouseEntered(with event: NSEvent) {
        mouse = point(for: event)
        needsDisplay = true
    }

    override func mouseExited(with event: NSEvent) {
        mouse = nil
        needsDisplay = true
    }

    override func mouseMoved(with event: NSEvent) {
        let location = point(for: event)
        mouse = location
        // The magnifier and the hint follow the cursor on this display; the
        // controller only needs to hear about it when the mode is one where
        // moving changes what is highlighted across every display.
        if controller?.mode != .region {
            controller?.mouseMoved(to: global(location))
        }
        needsDisplay = true
    }

    override func mouseDown(with event: NSEvent) {
        guard let controller else { return }
        let location = point(for: event)
        mouse = location
        switch controller.mode {
        case .region:
            dragOrigin = location
            draft = CGRect(origin: location, size: .zero)
        case .window:
            if let target = controller.window(at: global(location)) {
                controller.captureWindow(target)
            }
        case .display:
            controller.captureDisplay(at: index)
        }
        needsDisplay = true
    }

    override func mouseDragged(with event: NSEvent) {
        guard controller?.mode == .region, let origin = dragOrigin else { return }
        let location = point(for: event)
        mouse = location
        // A drag that runs off this display is clamped to it: the capture is a
        // crop of this display's frozen image and cannot span two of them.
        draft = CGRect(corner: origin, corner: location).intersection(bounds)
        needsDisplay = true
    }

    override func mouseUp(with event: NSEvent) {
        guard controller?.mode == .region, let rect = draft else { return }
        draft = nil
        dragOrigin = nil
        // A stray click is not a selection; leave the overlay up to try again.
        guard rect.width >= minimumSelection, rect.height >= minimumSelection else {
            needsDisplay = true
            return
        }
        controller?.captureRegion(global(rect), on: display)
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        guard let ctx = NSGraphicsContext.current?.cgContext, let controller else { return }

        let highlight = highlightRect(mode: controller.mode, controller: controller)
        drawDim(in: ctx, hole: highlight)
        if let highlight {
            drawSelectionBorder(highlight, in: ctx, mode: controller.mode)
            drawSizeReadout(for: highlight, in: ctx, mode: controller.mode)
        }
        guard mouse != nil, controller.mode == .region else { return }
        drawLoupe(in: ctx)
    }

    /// The undimmed area on this display, in view coordinates.
    private func highlightRect(mode: OverlayMode, controller: CaptureController) -> CGRect? {
        switch mode {
        case .region:
            return draft
        case .window:
            guard let frame = controller.hoveredWindow?.frame else { return nil }
            let clipped = local(frame).intersection(bounds)
            return clipped.isNull ? nil : clipped
        case .display:
            return controller.highlightedDisplay == index ? bounds : nil
        }
    }

    private func drawDim(in ctx: CGContext, hole: CGRect?) {
        let path = CGMutablePath()
        path.addRect(bounds)
        if let hole { path.addRect(hole) }
        ctx.saveGState()
        ctx.setFillColor(NSColor.black.withAlphaComponent(dimAlpha).cgColor)
        ctx.addPath(path)
        ctx.fillPath(using: .evenOdd)
        ctx.restoreGState()
    }

    private func drawSelectionBorder(_ rect: CGRect, in ctx: CGContext, mode: OverlayMode) {
        ctx.saveGState()
        // A dark line just outside the light one keeps the edge legible over
        // both white documents and dark terminals.
        ctx.setLineWidth(1)
        ctx.setStrokeColor(NSColor.black.withAlphaComponent(0.55).cgColor)
        ctx.stroke(rect.insetBy(dx: -1, dy: -1))
        ctx.setStrokeColor(mode == .region ? Style.text.cgColor : Style.highlight.cgColor)
        ctx.stroke(rect.insetBy(dx: 0.5, dy: 0.5))
        ctx.restoreGState()
    }

    private func drawSizeReadout(for rect: CGRect, in ctx: CGContext, mode: OverlayMode) {
        let scale = display.scale
        let text = String(format: "%.0f × %.0f", rect.width * scale, rect.height * scale)
        let size = pillSize(for: text)
        // Below the selection by preference, tucked inside it when the
        // selection is against the bottom of the display.
        var origin = CGPoint(x: rect.minX, y: rect.minY - size.height - 6)
        if origin.y < 6 { origin.y = rect.minY + 6 }
        origin.x = min(max(origin.x, 6), bounds.maxX - size.width - 6)
        drawPill(text, at: origin, in: ctx)
    }

    /// A zoomed inset of the frozen pixels under the cursor, so an edge can be
    /// found exactly rather than approximately.
    private func drawLoupe(in ctx: CGContext) {
        guard let mouse else { return }
        let scale = display.scale
        // Source square in image pixels, centred on the cursor. The image is
        // top-left origin, so y inverts against the view.
        let side = (loupeSide / loupeZoom) * scale
        let center = CGPoint(x: mouse.x * scale, y: (bounds.height - mouse.y) * scale)
        let source = CGRect(x: center.x - side / 2, y: center.y - side / 2,
                            width: side, height: side)
        let imageBounds = CGRect(x: 0, y: 0,
                                 width: CGFloat(display.image.width),
                                 height: CGFloat(display.image.height))
        let clamped = source.intersection(imageBounds).integral
        guard !clamped.isNull, clamped.width >= 1, clamped.height >= 1,
              let crop = display.image.cropping(to: clamped) else { return }

        var frame = CGRect(x: mouse.x + 20, y: mouse.y - loupeSide - 20,
                           width: loupeSide, height: loupeSide)
        if frame.maxX > bounds.maxX - 8 { frame.origin.x = mouse.x - loupeSide - 20 }
        if frame.minY < bounds.minY + 8 { frame.origin.y = mouse.y + 20 }

        // Clamping the source moves it off centre near a screen edge. Placing
        // the crop at the offset it actually came from keeps magnification
        // constant instead of stretching the remaining pixels to fill.
        let pointsPerPixel = loupeZoom / scale
        let destination = CGRect(
            x: frame.minX + (clamped.minX - source.minX) * pointsPerPixel,
            y: frame.maxY - (clamped.minY - source.minY) * pointsPerPixel
                - clamped.height * pointsPerPixel,
            width: clamped.width * pointsPerPixel,
            height: clamped.height * pointsPerPixel)

        ctx.saveGState()
        let clip = CGPath(roundedRect: frame, cornerWidth: 8, cornerHeight: 8, transform: nil)
        ctx.addPath(clip)
        ctx.clip()
        ctx.setFillColor(NSColor.black.cgColor)
        ctx.fill(frame)
        ctx.interpolationQuality = .none
        ctx.draw(crop, in: destination)

        // Crosshair on the pixel actually under the cursor.
        let pixel = CGRect(x: frame.midX - loupeZoom / 2, y: frame.midY - loupeZoom / 2,
                           width: loupeZoom, height: loupeZoom)
        ctx.setStrokeColor(NSColor(white: 1, alpha: 0.9).cgColor)
        ctx.setLineWidth(1)
        ctx.stroke(pixel)
        ctx.restoreGState()

        ctx.saveGState()
        ctx.setStrokeColor(NSColor(white: 1, alpha: 0.85).cgColor)
        ctx.setLineWidth(1)
        ctx.addPath(clip)
        ctx.strokePath()
        ctx.restoreGState()

        let global = global(mouse)
        let text = String(format: "%.0f, %.0f", global.x, Geometry.primaryHeight - global.y)
        let size = pillSize(for: text)
        drawPill(text, at: CGPoint(x: frame.midX - size.width / 2, y: frame.minY - size.height - 6),
                 in: ctx)
    }

    private func pillSize(for text: String) -> CGSize { Style.pillSize(for: text) }

    private func drawPill(_ text: String, at origin: CGPoint, in ctx: CGContext) {
        Style.drawPill(text, at: origin, in: ctx)
    }
}
