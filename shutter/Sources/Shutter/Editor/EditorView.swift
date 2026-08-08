import AppKit

/// The capture, plus the red rectangles drawn over it.
///
/// Annotations stay vector until export: they are stored in capture points and
/// mapped to the view on every draw, so resizing the window never resamples a
/// mark and the exported PNG is stroked at full resolution rather than being a
/// scaled-up copy of what the screen showed.
final class EditorView: NSView {
    let capture: CapturedImage
    private(set) var annotations: [Annotation] = []

    private var selected: Int?
    private var draft: CGRect?
    private var drag: DragState = .idle
    private var pendingUndo: [Annotation]?
    private var undoStack: [[Annotation]] = []
    private var redoStack: [[Annotation]] = []

    private let minimumAnnotation: CGFloat = 2

    private enum DragState {
        case idle
        case drawing(origin: CGPoint)
        /// `grab` is the cursor's offset from the rectangle's origin, so a
        /// moved rectangle does not jump to centre itself under the pointer.
        case moving(index: Int, grab: CGPoint)
    }

    init(capture: CapturedImage) {
        self.capture = capture
        super.init(frame: CGRect(origin: .zero, size: capture.pointSize))
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }


    // MARK: Layout

    /// Where the capture sits in the view, aspect-fit and centred.
    private var imageRect: CGRect {
        let size = capture.pointSize
        guard size.width > 0, size.height > 0, bounds.width > 0, bounds.height > 0 else {
            return .zero
        }
        let fit = min(bounds.width / size.width, bounds.height / size.height)
        let scaled = CGSize(width: size.width * fit, height: size.height * fit)
        return CGRect(x: (bounds.width - scaled.width) / 2,
                      y: (bounds.height - scaled.height) / 2,
                      width: scaled.width, height: scaled.height)
    }

    /// View points per capture point.
    private var viewScale: CGFloat {
        capture.pointSize.width > 0 ? imageRect.width / capture.pointSize.width : 1
    }

    private var captureBounds: CGRect { CGRect(origin: .zero, size: capture.pointSize) }

    private func capturePoint(_ point: CGPoint) -> CGPoint {
        CGPoint(x: (point.x - imageRect.minX) / viewScale,
                y: (point.y - imageRect.minY) / viewScale)
    }

    private func viewRect(_ rect: CGRect) -> CGRect {
        CGRect(x: imageRect.minX + rect.minX * viewScale,
               y: imageRect.minY + rect.minY * viewScale,
               width: rect.width * viewScale, height: rect.height * viewScale)
    }

    // MARK: Editing

    private func mutate(_ change: () -> Void) {
        undoStack.append(annotations)
        redoStack.removeAll()
        change()
        needsDisplay = true
    }

    func undo() {
        guard let previous = undoStack.popLast() else { return }
        redoStack.append(annotations)
        annotations = previous
        selected = nil
        needsDisplay = true
    }

    func redo() {
        guard let next = redoStack.popLast() else { return }
        undoStack.append(annotations)
        annotations = next
        selected = nil
        needsDisplay = true
    }

    func deleteSelection() {
        guard let index = selected, annotations.indices.contains(index) else { return }
        mutate {
            annotations.remove(at: index)
            selected = nil
        }
    }

    /// Index of the rectangle whose border is under `point`, topmost first.
    ///
    /// The border rather than the fill: drawing a rectangle inside another one
    /// is routine, and hit-testing the fill would make the inner one
    /// unreachable.
    private func annotationIndex(at point: CGPoint) -> Int? {
        let band = max(6 / viewScale, Annotation.strokeWidth)
        for index in annotations.indices.reversed() {
            let rect = annotations[index].rect
            let outer = rect.insetBy(dx: -band, dy: -band)
            guard outer.contains(point) else { continue }
            let inner = rect.insetBy(dx: band, dy: band)
            let insideHole = inner.width > 0 && inner.height > 0 && inner.contains(point)
            if !insideHole { return index }
        }
        return nil
    }

    // MARK: Events

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func resetCursorRects() {
        addCursorRect(imageRect, cursor: .crosshair)
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        // The crosshair only covers the image, which moves when the window does.
        window?.invalidateCursorRects(for: self)
    }

    override func mouseDown(with event: NSEvent) {
        let point = capturePoint(convert(event.locationInWindow, from: nil))
        if let index = annotationIndex(at: point) {
            selected = index
            pendingUndo = annotations
            let origin = annotations[index].rect.origin
            drag = .moving(index: index,
                           grab: CGPoint(x: point.x - origin.x, y: point.y - origin.y))
        } else {
            selected = nil
            drag = .drawing(origin: point)
            draft = CGRect(origin: point, size: .zero)
        }
        needsDisplay = true
    }

    override func mouseDragged(with event: NSEvent) {
        let point = capturePoint(convert(event.locationInWindow, from: nil))
        switch drag {
        case .drawing(let origin):
            draft = CGRect(corner: origin, corner: point).intersection(captureBounds)
        case .moving(let index, let grab):
            guard annotations.indices.contains(index) else { return }
            var rect = annotations[index].rect
            rect.origin = CGPoint(
                x: min(max(point.x - grab.x, 0), captureBounds.width - rect.width),
                y: min(max(point.y - grab.y, 0), captureBounds.height - rect.height))
            annotations[index].rect = rect
        case .idle:
            return
        }
        needsDisplay = true
    }

    override func mouseUp(with event: NSEvent) {
        switch drag {
        case .drawing:
            if let rect = draft, rect.width >= minimumAnnotation, rect.height >= minimumAnnotation {
                mutate {
                    annotations.append(Annotation(rect: rect))
                    selected = annotations.count - 1
                }
            }
            draft = nil
        case .moving:
            // Only a drag that actually moved something is worth an undo step.
            if let pending = pendingUndo, pending != annotations {
                undoStack.append(pending)
                redoStack.removeAll()
            }
            pendingUndo = nil
        case .idle:
            break
        }
        drag = .idle
        needsDisplay = true
    }

    override func keyDown(with event: NSEvent) {
        switch event.keyCode {
        case 51, 117: // delete, forward delete
            deleteSelection()
        default:
            super.keyDown(with: event)
        }
    }

    /// Undo is handled here rather than through a menu item: an `LSUIElement`
    /// app has no menu bar to hang one on.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        guard modifiers.contains(.command), event.charactersIgnoringModifiers?.lowercased() == "z"
        else { return false }
        if modifiers.contains(.shift) { redo() } else { undo() }
        return true
    }

    // MARK: Drawing

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        // The page colour is a dynamic token resolved at draw time.
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            guard let ctx = NSGraphicsContext.current?.cgContext else { return }

            // The capture sits directly on the paper: no frame, no mat, no
            // drop shadow. It is content, and content is flat (rules 1 and 2).
            NotesStyle.bg.setFill()
            bounds.fill()

            ctx.interpolationQuality = .high
            ctx.draw(capture.image, in: imageRect)

            ctx.setStrokeColor(Annotation.strokeColor.cgColor)
            ctx.setLineWidth(Annotation.strokeWidth * viewScale)
            ctx.setLineJoin(.miter)
            for annotation in annotations {
                ctx.stroke(viewRect(annotation.rect))
            }
            if let draft {
                ctx.stroke(viewRect(draft))
            }

            if let selected, annotations.indices.contains(selected) {
                drawSelectionMarker(around: viewRect(annotations[selected].rect), in: ctx)
            }
        }
    }

    /// A selected mark can be moved and deleted, so it is interactive, and the
    /// accent is what marks interactive things (rule 4).
    private func drawSelectionMarker(around rect: CGRect, in ctx: CGContext) {
        ctx.saveGState()
        ctx.setStrokeColor(NotesStyle.accent.cgColor)
        ctx.setLineWidth(1)
        ctx.setLineDash(phase: 0, lengths: [4, 3])
        let inset = Annotation.strokeWidth * viewScale / 2 + 2
        ctx.stroke(rect.insetBy(dx: -inset, dy: -inset))
        ctx.restoreGState()
    }
}
