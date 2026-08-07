import AppKit

/// The mouse-driven face of the stack: a flat panel listing every managed
/// window, one dense row each, click to switch.
///
/// The panel never becomes key. Taking key focus would deactivate the very
/// window the user is switching away from, and the contract is that the front
/// window keeps focus until a switch is actually committed — so this is a
/// non-activating panel that reads the stack and reports a choice back, nothing
/// more. Membership is read fresh on each summon, so there is no second copy of
/// the stack's state to keep in sync.
final class SwitcherPanel {
    private let stack: WindowStack
    private var panel: NSPanel?
    /// Watches for clicks landing outside the panel, which dismiss it.
    private var outsideClickMonitor: Any?

    var isVisible: Bool { panel?.isVisible ?? false }

    init(stack: WindowStack) {
        self.stack = stack
    }

    func toggle() {
        isVisible ? dismiss() : show()
    }

    func show() {
        dismiss()
        guard let screen = Displays.screen(matching: stack.selectedUUID) ?? NSScreen.main
        else { return }

        let entries = stack.windowList()
        let content = SwitcherContentView(entries: entries) { [weak self] id in
            // Dismiss first, so the panel isn't left floating over the window
            // it just brought to the front.
            self?.dismiss()
            self?.stack.switchToWindow(id: id)
        }
        content.layOut(maxHeight: screen.visibleFrame.height * 0.7)

        let panel = NSPanel(contentRect: content.frame,
                            styleMask: [.borderless, .nonactivatingPanel],
                            backing: .buffered, defer: false)
        panel.contentView = content
        panel.isFloatingPanel = true
        panel.becomesKeyOnlyIfNeeded = true
        panel.hidesOnDeactivate = false
        panel.level = .popUpMenu          // above ordinary windows, below alerts
        panel.isOpaque = true
        panel.hasShadow = false           // design guide: no soft drop shadows
        panel.backgroundColor = Ink.background
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]
        panel.setFrame(centred(content.frame.size, on: screen), display: true)
        panel.orderFrontRegardless()      // ordering front must not activate us
        self.panel = panel

        // Global monitors only see other apps' events, so a click reported here
        // is by definition outside the panel.
        outsideClickMonitor = NSEvent.addGlobalMonitorForEvents(
            matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown]
        ) { [weak self] _ in
            self?.dismiss()
        }
    }

    func dismiss() {
        if let outsideClickMonitor {
            NSEvent.removeMonitor(outsideClickMonitor)
            self.outsideClickMonitor = nil
        }
        panel?.orderOut(nil)
        panel = nil
    }

    /// Centred horizontally, and a little above centre vertically — where the
    /// eye already is, rather than dead centre over the stage.
    private func centred(_ size: NSSize, on screen: NSScreen) -> NSRect {
        let area = screen.visibleFrame
        return NSRect(x: area.midX - size.width / 2,
                      y: area.midY - size.height / 2 + area.height * 0.08,
                      width: size.width, height: size.height)
    }
}

// MARK: - Palette and type

/// The dark "terminal" discipline of the design guide: flat fills, hairline
/// rules, one signal accent, no glow and no blur.
private enum Ink {
    static let background = NSColor(srgbRed: 0.071, green: 0.071, blue: 0.071, alpha: 1) // #121212
    static let hairline = NSColor(srgbRed: 0.200, green: 0.200, blue: 0.200, alpha: 1)   // #333333
    static let primary = NSColor(srgbRed: 0.910, green: 0.910, blue: 0.902, alpha: 1)    // #E8E8E6
    static let secondary = NSColor(srgbRed: 0.541, green: 0.541, blue: 0.522, alpha: 1)  // #8A8A85
    static let accent = NSColor(srgbRed: 0.302, green: 0.671, blue: 0.969, alpha: 1)     // #4DABF7
    static let hover = NSColor(srgbRed: 0.110, green: 0.110, blue: 0.110, alpha: 1)      // #1C1C1C
}

/// Fixed metrics on a 4pt scale — the design guide's modular spacing, so nothing
/// in the panel floats off-grid.
private enum Metric {
    static let width: CGFloat = 460
    static let row: CGFloat = 40
    static let header: CGFloat = 30
    static let footer: CGFloat = 26
    static let pad: CGFloat = 12
    static let digitColumn: CGFloat = 28
    static let icon: CGFloat = 16
}

// MARK: - Content

private final class SwitcherContentView: NSView {
    private let entries: [WindowStack.WindowEntry]
    private let onPick: (CGWindowID) -> Void
    private let scrollView = NSScrollView()
    private let list = FlippedView()

    init(entries: [WindowStack.WindowEntry], onPick: @escaping (CGWindowID) -> Void) {
        self.entries = entries
        self.onPick = onPick
        super.init(frame: .zero)
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.documentView = list
        addSubview(scrollView)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var isFlipped: Bool { true }

    /// Size to the content, capped so a long list scrolls instead of running off
    /// the screen.
    func layOut(maxHeight: CGFloat) {
        let listHeight = CGFloat(max(entries.count, 1)) * Metric.row
        let room = max(Metric.row * 3, maxHeight - Metric.header - Metric.footer)
        let visible = min(listHeight, room)

        frame = NSRect(x: 0, y: 0, width: Metric.width,
                       height: Metric.header + visible + Metric.footer)
        scrollView.frame = NSRect(x: 0, y: Metric.header, width: Metric.width, height: visible)
        list.frame = NSRect(x: 0, y: 0, width: Metric.width, height: listHeight)

        list.subviews.forEach { $0.removeFromSuperview() }
        for (i, entry) in entries.enumerated() {
            let row = SwitcherRow(entry: entry, onPick: onPick)
            row.frame = NSRect(x: 0, y: CGFloat(i) * Metric.row,
                               width: Metric.width, height: Metric.row)
            list.addSubview(row)
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        Ink.background.setFill()
        bounds.fill()

        let count = entries.count
        let header = NSRect(x: Metric.pad, y: 9, width: bounds.width - Metric.pad * 2, height: 14)
        Typeface.draw("TILER — \(count) WINDOW\(count == 1 ? "" : "S")",
                  Typeface.mono(11, bold: true), Ink.secondary, in: header)

        Ink.hairline.setFill()
        NSRect(x: 0, y: Metric.header - 1, width: bounds.width, height: 1).fill()
        NSRect(x: 0, y: bounds.maxY - Metric.footer, width: bounds.width, height: 1).fill()

        let footer = NSRect(x: Metric.pad, y: bounds.maxY - Metric.footer + 7,
                            width: bounds.width - Metric.pad * 2, height: 14)
        Typeface.draw("CLICK TO SWITCH · ESC TO DISMISS", Typeface.mono(11), Ink.secondary, in: footer)

        if entries.isEmpty {
            let empty = NSRect(x: Metric.pad, y: Metric.header + 13,
                               width: bounds.width - Metric.pad * 2, height: 16)
            Typeface.draw("NO WINDOWS MANAGED", Typeface.mono(12), Ink.secondary, in: empty)
        }

        // Crisp 1px frame, inset by half a point so it lands on the pixel.
        Ink.hairline.setStroke()
        let border = NSBezierPath(rect: bounds.insetBy(dx: 0.5, dy: 0.5))
        border.lineWidth = 1
        border.stroke()
    }
}

/// A container whose coordinates run top-down, matching the panels it sits in.
/// Shared with the command panel's chip rows.
final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

// MARK: - Row

private final class SwitcherRow: NSView {
    private let entry: WindowStack.WindowEntry
    private let onPick: (CGWindowID) -> Void
    private let appName: String
    private let icon: NSImage?
    private var hovering = false
    private var tracking: NSTrackingArea?
    private var lastTrackingRect: NSRect = .zero

    init(entry: WindowStack.WindowEntry, onPick: @escaping (CGWindowID) -> Void) {
        self.entry = entry
        self.onPick = onPick
        let app = NSRunningApplication(processIdentifier: entry.pid)
        self.appName = app?.localizedName ?? "Window \(entry.id)"
        self.icon = app?.icon
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var isFlipped: Bool { true }

    // `.activeAlways`: the panel is deliberately never key, so hover has to
    // track while Tiler is an inactive app.
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if tracking != nil, lastTrackingRect == bounds { return }
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: bounds,
                                  options: [.mouseEnteredAndExited, .activeAlways],
                                  owner: self)
        addTrackingArea(area)
        tracking = area
        lastTrackingRect = bounds
    }

    override func mouseEntered(with event: NSEvent) {
        hovering = true
        needsDisplay = true
    }

    override func mouseExited(with event: NSEvent) {
        hovering = false
        needsDisplay = true
    }

    override func mouseUp(with event: NSEvent) {
        guard bounds.contains(convert(event.locationInWindow, from: nil)) else { return }
        onPick(entry.id)
    }

    override func draw(_ dirtyRect: NSRect) {
        if hovering {
            Ink.hover.setFill()
            bounds.fill()
        }
        if entry.isFront {
            // The window holding the stage, marked permanently — an affordance
            // the user can see without hovering to discover it.
            Ink.accent.setFill()
            NSRect(x: 0, y: 0, width: 2, height: bounds.height).fill()
        }
        Ink.hairline.setFill()
        NSRect(x: 0, y: bounds.maxY - 1, width: bounds.width, height: 1).fill()

        let digit = NSRect(x: Metric.pad, y: 8, width: Metric.digitColumn, height: 14)
        Typeface.draw(entry.number.map { "⌘\($0)" } ?? " ·",
                  Typeface.mono(12), entry.number == nil ? Ink.secondary : Ink.accent, in: digit)

        let iconX = Metric.pad + Metric.digitColumn + 6
        icon?.draw(in: NSRect(x: iconX, y: 7, width: Metric.icon, height: Metric.icon))

        let textX = iconX + Metric.icon + 8
        let textWidth = bounds.width - textX - Metric.pad
        Typeface.draw(appName, Typeface.mono(13, bold: true),
                  entry.isFront ? Ink.accent : Ink.primary,
                  in: NSRect(x: textX, y: 5, width: textWidth, height: 16))
        Typeface.draw(entry.title, Typeface.mono(11), Ink.secondary,
                  in: NSRect(x: textX, y: 22, width: textWidth, height: 14))
    }
}
