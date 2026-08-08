import AppKit
import QuartzCore

/// The notes app's button: a filled key with a hard 2px outline and a solid
/// offset shadow, which slides down into that shadow when pressed.
///
/// Depth here is the interactivity signal (rule 2) — it is the only thing in
/// the editor allowed an outline or a shadow, because everything else on the
/// page is static content separated by whitespace alone (rule 1).
///
/// The press is animated by redrawing rather than by a layer transform. The
/// shadow has to stay put while the face moves onto it, and driving one scalar
/// through the stylesheet's easing keeps both halves in step without a second
/// layer to keep aligned.
final class PaperButton: NSButton {
    enum Variant {
        /// `.button` — accent fill.
        case primary
        /// `.button--secondary` — surface fill, accent text.
        case secondary
        /// `.button--danger` — surface fill, danger text.
        case danger
    }

    private let variant: Variant
    private var isHovered = false

    /// 0 is fully up, 1 is fully pressed.
    private var press: CGFloat = 0
    private var pressFrom: CGFloat = 0
    private var pressTarget: CGFloat = 0
    private var pressStart: CFTimeInterval = 0
    private var pressTimer: Timer?

    init(title: String, variant: Variant, key: String, modifiers: NSEvent.ModifierFlags,
         target: AnyObject?, action: Selector) {
        self.variant = variant
        super.init(frame: .zero)
        self.title = title
        self.target = target
        self.action = action
        keyEquivalent = key
        keyEquivalentModifierMask = modifiers
        isBordered = false
        focusRingType = .none
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    deinit { pressTimer?.invalidate() }

    // MARK: Layout

    /// The face, which leaves room below and to the right for the shadow it
    /// sits on top of.
    private var faceRect: CGRect {
        CGRect(x: 0, y: NotesStyle.shadowOffset,
               width: bounds.width - NotesStyle.shadowOffset,
               height: bounds.height - NotesStyle.shadowOffset)
    }

    override var intrinsicContentSize: NSSize {
        let width = NSAttributedString(string: title, attributes: [.font: NotesStyle.button])
            .size().width
        return NSSize(
            width: ceil(width) + NotesStyle.buttonPaddingH * 2
                + NotesStyle.borderWidth * 2 + NotesStyle.shadowOffset,
            height: NotesStyle.buttonHeight + NotesStyle.shadowOffset)
    }

    // MARK: Interaction

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(
            rect: .zero,
            options: [.mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
            owner: self))
    }

    override func mouseEntered(with event: NSEvent) {
        isHovered = true
        needsDisplay = true
    }

    override func mouseExited(with event: NSEvent) {
        isHovered = false
        needsDisplay = true
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .pointingHand)
    }

    override var isHighlighted: Bool {
        didSet {
            guard isHighlighted != oldValue else { return }
            animatePress(to: isHighlighted ? 1 : 0)
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        // Dynamic tokens resolve at draw time, so a scheme change needs a redraw.
        needsDisplay = true
    }

    private func animatePress(to target: CGFloat) {
        pressTimer?.invalidate()
        pressFrom = press
        pressTarget = target
        pressStart = CACurrentMediaTime()
        pressTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 60, repeats: true) { [weak self] timer in
            MainActor.assumeIsolated {
                guard let self else {
                    timer.invalidate()
                    return
                }
                let fraction = min((CACurrentMediaTime() - self.pressStart)
                                   / NotesStyle.motionDuration, 1)
                let eased = CGFloat(NotesStyle.motionEasing.value(at: fraction))
                self.press = self.pressFrom + (self.pressTarget - self.pressFrom) * eased
                if fraction >= 1 {
                    self.press = self.pressTarget
                    timer.invalidate()
                    self.pressTimer = nil
                }
                self.needsDisplay = true
            }
        }
    }

    // MARK: Drawing

    override var allowsVibrancy: Bool { false }

    private var faceFill: NSColor {
        switch variant {
        case .primary: return NotesStyle.accent
        case .secondary, .danger: return NotesStyle.fill
        }
    }

    private var titleColor: NSColor {
        switch variant {
        case .primary: return NotesStyle.accentContrast
        case .secondary: return NotesStyle.accent
        case .danger: return NotesStyle.danger
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        // Resolves the dynamic tokens against this view's scheme; without it
        // `.cgColor` would hand back whichever variant was current last.
        effectiveAppearance.performAsCurrentDrawingAppearance {
            guard let ctx = NSGraphicsContext.current?.cgContext else { return }
            let radius = NotesStyle.cornerRadius
            let face = faceRect

            // The shadow stays where it is; the face slides onto it and covers
            // it exactly, which is what `box-shadow: 0 0 0` looks like.
            let shadow = face.offsetBy(dx: NotesStyle.shadowOffset, dy: -NotesStyle.shadowOffset)
            ctx.setFillColor(NotesStyle.ink.cgColor)
            ctx.addPath(CGPath(roundedRect: shadow, cornerWidth: radius,
                               cornerHeight: radius, transform: nil))
            ctx.fillPath()

            let moved = face.offsetBy(dx: NotesStyle.shadowOffset * press,
                                      dy: -NotesStyle.shadowOffset * press)
            // Hover is a brightness filter, and it is cleared while pressed so
            // the two never compound.
            let dim = (isHovered && press == 0) ? 0.95 : 1.0
            let border = NotesStyle.borderWidth
            let path = CGPath(roundedRect: moved.insetBy(dx: border / 2, dy: border / 2),
                              cornerWidth: radius, cornerHeight: radius, transform: nil)
            ctx.setFillColor(NotesStyle.brightness(faceFill, dim).cgColor)
            ctx.addPath(path)
            ctx.fillPath()
            ctx.setStrokeColor(NotesStyle.brightness(NotesStyle.ink, dim).cgColor)
            ctx.setLineWidth(border)
            ctx.addPath(path)
            ctx.strokePath()

            let string = NSAttributedString(string: title, attributes: [
                .font: NotesStyle.button,
                .foregroundColor: NotesStyle.brightness(titleColor, dim),
            ])
            let size = string.size()
            string.draw(at: CGPoint(x: moved.midX - size.width / 2,
                                    y: moved.midY - size.height / 2))
        }
    }
}
