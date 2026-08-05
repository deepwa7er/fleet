import AppKit

/// Tiler's menu, as a window rather than a menu.
///
/// A borderless panel that opens at the pointer and carries everything the ◎
/// menu carries: the managed windows, the display picker, restore frames, start
/// at login, quit. Borderless means it has no title bar, and therefore no
/// close/minimise/zoom buttons — there is nothing to suppress, they simply do
/// not exist on a window with no frame.
///
/// Like the switcher, this panel never becomes key. Taking key focus would
/// deactivate the very window the user is about to act on, and the contract is
/// that the front window keeps focus until a switch is actually committed. So
/// it is a non-activating panel that reads the stack and reports a choice back.
/// Escape is handled by the event tap, for the same reason.
final class CommandPanel {
    private let stack: WindowStack
    private var panel: NSPanel?
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

        let content = CommandContentView(
            stack: stack,
            onPick: { [weak self] id in
                // Dismiss first, so the panel isn't left floating over the
                // window it just brought to the front.
                self?.dismiss()
                self?.stack.switchToWindow(id: id)
            },
            onReorder: { [weak self] ids in
                // Position is the numbering, so a drop rewrites the digits and
                // the rebuilt list comes back in exactly the dropped order.
                self?.stack.renumber(order: ids)
                self?.reload()
            },
            onDisplay: { [weak self] uuid in
                self?.stack.select(displayUUID: uuid)
                StateLog.append("display -> \(uuid)")
                self?.reload()
            },
            onRestore: { [weak self] in
                self?.dismiss()
                self?.stack.restoreAll()
            },
            onLoginToggle: { [weak self] in
                self?.toggleStartAtLogin()
                self?.reload()
            },
            onQuit: { NSApp.terminate(nil) })

        let anchor = NSEvent.mouseLocation
        let screen = NSScreen.screens.first { $0.frame.contains(anchor) } ?? NSScreen.main
        content.layOut(maxHeight: (screen?.visibleFrame.height ?? 800) * 0.75)

        let panel = NSPanel(contentRect: content.frame,
                            styleMask: [.borderless, .nonactivatingPanel],
                            backing: .buffered, defer: false)
        panel.contentView = content
        panel.isFloatingPanel = true
        panel.becomesKeyOnlyIfNeeded = true
        panel.hidesOnDeactivate = false
        panel.level = .popUpMenu          // above ordinary windows, below alerts
        // The rounded body is drawn by the content view, so the window itself
        // has to be transparent or its square corners would show through.
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false           // flat: the hairline edge is the whole frame
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]
        panel.setFrame(frame(for: content.frame.size, at: anchor, on: screen), display: true)
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

    /// Rebuild in place after an action that changes what the panel shows,
    /// keeping it where the user put it rather than re-anchoring to a pointer
    /// that has since moved.
    private func reload() {
        guard let panel, let content = panel.contentView as? CommandContentView,
              let screen = panel.screen ?? NSScreen.main
        else { return }
        let topLeft = CGPoint(x: panel.frame.minX, y: panel.frame.maxY)
        content.layOut(maxHeight: screen.visibleFrame.height * 0.75)
        panel.setFrame(NSRect(x: topLeft.x, y: topLeft.y - content.frame.height,
                              width: content.frame.width, height: content.frame.height),
                       display: true)
    }

    /// Anchored below and right of the pointer like a context menu, then pushed
    /// back inside the screen if that would hang it off an edge.
    private func frame(for size: NSSize, at anchor: CGPoint, on screen: NSScreen?) -> NSRect {
        let area = screen?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        let inset: CGFloat = 8
        var origin = CGPoint(x: anchor.x + 12, y: anchor.y - size.height - 12)
        origin.x = min(max(origin.x, area.minX + inset), area.maxX - size.width - inset)
        origin.y = min(max(origin.y, area.minY + inset), area.maxY - size.height - inset)
        return NSRect(origin: origin, size: size)
    }

    private func toggleStartAtLogin() {
        guard LoginItem.status != .requiresApproval else {
            LoginItem.openSystemSettings() // only the user can clear that state
            return
        }
        let enabling = !LoginItem.isEnabled
        do {
            try LoginItem.setEnabled(enabling)
            StateLog.append("start at login -> \(enabling)")
        } catch {
            StateLog.append("start at login \(enabling ? "register" : "unregister") failed: \(error)")
        }
    }
}

// MARK: - Content

private final class CommandContentView: NSView {
    private let stack: WindowStack
    private let onPick: (CGWindowID) -> Void
    private let onDisplay: (String) -> Void
    private let onRestore: () -> Void
    private let onLoginToggle: () -> Void
    private let onQuit: () -> Void

    private let scrollView = NSScrollView()
    private let list: WindowListView
    private var chips: [Chip] = []
    private var entries: [WindowStack.WindowEntry] = []
    private var displayName = ""

    /// Where the sections landed in the last layout, so `draw` can put the
    /// labels and rules in the same places without recomputing them.
    private var listFrame = NSRect.zero
    private var displaysLabelY: CGFloat = 0
    private var actionsLabelY: CGFloat = 0
    private var hintY: CGFloat = 0

    init(stack: WindowStack,
         onPick: @escaping (CGWindowID) -> Void,
         onReorder: @escaping ([CGWindowID]) -> Void,
         onDisplay: @escaping (String) -> Void,
         onRestore: @escaping () -> Void,
         onLoginToggle: @escaping () -> Void,
         onQuit: @escaping () -> Void) {
        self.stack = stack
        self.onPick = onPick
        self.list = WindowListView(onPick: onPick, onReorder: onReorder)
        self.onDisplay = onDisplay
        self.onRestore = onRestore
        self.onLoginToggle = onLoginToggle
        self.onQuit = onQuit
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

    /// Dragging anywhere that isn't a row or a chip moves the panel.
    ///
    /// A borderless window has no title bar to grab, so the body has to be the
    /// handle. `mouseDown` only reaches this view when no subview claimed the
    /// event, which is exactly the split we want: rows keep their own
    /// drag-to-reorder, chips keep their clicks, and everything else — the
    /// header, the section labels, the padding — moves the window.
    override func mouseDown(with event: NSEvent) {
        window?.performDrag(with: event)
    }

    // MARK: Layout

    func layOut(maxHeight: CGFloat) {
        entries = stack.windowList()
        displayName = Displays.screen(matching: stack.selectedUUID)?.localizedName ?? "—"

        chips.forEach { $0.removeFromSuperview() }
        chips.removeAll()

        let width = Panel.width
        let inner = width - Panel.pad * 2
        var y = Panel.pad

        // Header readout.
        y += 16 + Panel.gap

        // Window list, capped so a long stack scrolls instead of running off
        // the screen. The rest of the panel is a fixed height, so the cap is
        // whatever is left over.
        let fixedBelow: CGFloat = 16 + Panel.gap + Panel.chipHeight + Panel.pad   // displays
            + 16 + Panel.gap + Panel.chipHeight                                   // actions
            + Panel.pad + 14 + Panel.pad                                          // hint
        let listHeight = CGFloat(max(entries.count, 1)) * Panel.row
        let room = max(Panel.row * 2, maxHeight - y - fixedBelow)
        let visible = min(listHeight, room)

        listFrame = NSRect(x: 0, y: y, width: width, height: visible)
        scrollView.frame = listFrame
        list.frame = NSRect(x: 0, y: 0, width: width, height: listHeight)
        list.setEntries(entries)
        y += visible + Panel.pad

        // Display picker: every connected screen as a chip, so there is no
        // submenu anywhere in the panel.
        displaysLabelY = y
        y += 16 + Panel.gap
        let selected = Displays.screen(matching: stack.selectedUUID).flatMap(Displays.uuid(of:))
        var chipRow: [Chip] = []
        for screen in NSScreen.screens {
            guard let uuid = Displays.uuid(of: screen) else { continue }
            let chip = Chip(title: screen.localizedName, isSelected: uuid == selected) {
                [weak self] in self?.onDisplay(uuid)
            }
            chipRow.append(chip)
        }
        y = place(chipRow, from: y, width: inner)
        y += Panel.pad

        // Actions.
        actionsLabelY = y
        y += 16 + Panel.gap
        let loginTitle: String
        switch LoginItem.status {
        case .requiresApproval: loginTitle = "Login: approve in Settings"
        case .enabled: loginTitle = "Start at login: on"
        default: loginTitle = "Start at login: off"
        }
        y = place([
            Chip(title: "Restore frames", isSelected: false, onClick: onRestore),
            Chip(title: loginTitle, isSelected: LoginItem.isEnabled, onClick: onLoginToggle),
            Chip(title: "Quit", isSelected: false, onClick: onQuit),
        ], from: y, width: inner)

        y += Panel.pad
        hintY = y
        y += 14 + Panel.pad

        frame = NSRect(x: 0, y: 0, width: width, height: y)
    }

    /// Lay chips left to right, wrapping when the row is full. Returns the y
    /// just past the last row.
    private func place(_ row: [Chip], from top: CGFloat, width: CGFloat) -> CGFloat {
        var x = Panel.pad
        var y = top
        for chip in row {
            let size = chip.intrinsicContentSize
            if x > Panel.pad, x + size.width > Panel.pad + width {
                x = Panel.pad
                y += Panel.chipHeight + Panel.gap
            }
            chip.frame = NSRect(x: x, y: y, width: size.width, height: Panel.chipHeight)
            addSubview(chip)
            chips.append(chip)
            x += size.width + Panel.gap
        }
        return y + Panel.chipHeight
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        let body = bounds.insetBy(dx: 0.5, dy: 0.5)
        Panel.fill(body, radius: Panel.radius, with: Panel.body)

        let count = entries.count
        Typeface.draw("TILER · \(count) WINDOW\(count == 1 ? "" : "S")",
                      Typeface.mono(11, bold: true), Panel.muted,
                      in: NSRect(x: Panel.pad, y: Panel.pad,
                                 width: bounds.width - Panel.pad * 2, height: 14))
        let display = displayName.uppercased()
        let displayWidth = Typeface.width(display, Typeface.mono(11))
        Typeface.draw(display, Typeface.mono(11), Panel.faint,
                      in: NSRect(x: bounds.maxX - Panel.pad - displayWidth, y: Panel.pad,
                                 width: displayWidth, height: 14))

        if entries.isEmpty {
            Typeface.draw("NO WINDOWS MANAGED", Typeface.mono(12), Panel.faint,
                          in: NSRect(x: Panel.pad, y: listFrame.minY + 10,
                                     width: bounds.width - Panel.pad * 2, height: 16))
        }

        label("DISPLAY", at: displaysLabelY)
        label("ACTIONS", at: actionsLabelY)

        Typeface.draw("DRAG TO REORDER · ⌘1–9 SWITCH · ESC CLOSE",
                      Typeface.mono(10), Panel.faint,
                      in: NSRect(x: Panel.pad, y: hintY,
                                 width: bounds.width - Panel.pad * 2, height: 14))

        // The bezel, drawn last so nothing overlaps it. Inset by half a point
        // so the one-pixel stroke lands on the pixel instead of straddling it.
        Panel.edge.setStroke()
        let bezel = NSBezierPath(roundedRect: body, xRadius: Panel.radius, yRadius: Panel.radius)
        bezel.lineWidth = 1
        bezel.stroke()
    }

    private func label(_ text: String, at y: CGFloat) {
        Typeface.draw(text, Typeface.mono(10, bold: true), Panel.faint,
                      in: NSRect(x: Panel.pad, y: y, width: 200, height: 14))
        // A short rule beside the label, not across the panel: the sections are
        // separated by space, and this only marks where one starts.
        Panel.hairline.setFill()
        let textWidth = Typeface.width(text, Typeface.mono(10, bold: true))
        NSRect(x: Panel.pad + textWidth + 8, y: y + 6,
               width: bounds.width - Panel.pad * 2 - textWidth - 8, height: 1).fill()
    }
}

// MARK: - Window list

/// The reorderable window column.
///
/// Row position *is* the ⌘-digit — the first row is ⌘1 — so dragging a row is
/// how numbers are reassigned. The list owns the ordering because a drag is a
/// statement about the whole column, not about one row.
private final class WindowListView: NSView {
    private let onPick: (CGWindowID) -> Void
    private let onReorder: ([CGWindowID]) -> Void
    private var rows: [WindowRow] = []

    private var dragged: WindowRow?
    /// Where inside the row the pointer took hold, so it doesn't jump to centre
    /// itself under the cursor.
    private var grab: CGFloat = 0
    private var target = 0

    init(onPick: @escaping (CGWindowID) -> Void, onReorder: @escaping ([CGWindowID]) -> Void) {
        self.onPick = onPick
        self.onReorder = onReorder
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var isFlipped: Bool { true }

    func setEntries(_ entries: [WindowStack.WindowEntry]) {
        dragged = nil
        rows.forEach { $0.removeFromSuperview() }
        rows = entries.map { entry in
            let row = WindowRow(entry: entry)
            row.delegate = self
            addSubview(row)
            return row
        }
        layoutRows()
    }

    /// Stack the rows top to bottom, optionally leaving one slot empty — the
    /// gap the dragged row will drop into.
    private func layoutRows(gapAt gap: Int? = nil) {
        var slot = 0
        for row in rows where row !== dragged {
            if slot == gap { slot += 1 }
            row.frame = NSRect(x: 0, y: CGFloat(slot) * Panel.row,
                               width: bounds.width, height: Panel.row)
            slot += 1
        }
    }
}

extension WindowListView: WindowRowDelegate {
    func rowWasClicked(_ row: WindowRow) {
        onPick(row.windowID)
    }

    func rowDidBeginDrag(_ row: WindowRow, grabOffset: CGFloat) {
        dragged = row
        grab = grabOffset
        target = rows.firstIndex { $0 === row } ?? 0
        row.isDragging = true
        // Above its neighbours, so it reads as lifted off the column.
        addSubview(row, positioned: .above, relativeTo: nil)
    }

    func rowDidDrag(_ row: WindowRow, to point: CGPoint) {
        guard dragged === row else { return }
        let top = min(max(point.y - grab, 0), max(bounds.height - Panel.row, 0))
        row.frame.origin.y = top
        // The slot the row's own midpoint is over.
        let slot = Int(((top + Panel.row / 2) / Panel.row).rounded(.down))
        let clamped = min(max(slot, 0), max(rows.count - 1, 0))
        guard clamped != target else { return }
        target = clamped
        layoutRows(gapAt: target)
    }

    func rowDidEndDrag(_ row: WindowRow) {
        guard dragged === row else { return }
        dragged = nil
        row.isDragging = false
        var order = rows.filter { $0 !== row }
        order.insert(row, at: min(target, order.count))
        rows = order
        layoutRows()
        onReorder(rows.map(\.windowID))
    }
}

// MARK: - Window row

@MainActor
private protocol WindowRowDelegate: AnyObject {
    func rowWasClicked(_ row: WindowRow)
    func rowDidBeginDrag(_ row: WindowRow, grabOffset: CGFloat)
    /// `point` is in the list's coordinates — the row itself is moving, so its
    /// own coordinate space is not a fixed reference during a drag.
    func rowDidDrag(_ row: WindowRow, to point: CGPoint)
    func rowDidEndDrag(_ row: WindowRow)
}

private final class WindowRow: NSView {
    let windowID: CGWindowID
    weak var delegate: (any WindowRowDelegate)?
    var isDragging = false { didSet { needsDisplay = true } }

    private let entry: WindowStack.WindowEntry
    private let appName: String
    private let icon: NSImage?
    private var hovering = false
    private var tracking: NSTrackingArea?
    private var pressPoint: CGPoint?
    private var passedThreshold = false

    /// How far the pointer must travel before a click becomes a drag.
    private let dragThreshold: CGFloat = 4

    init(entry: WindowStack.WindowEntry) {
        self.entry = entry
        self.windowID = entry.id
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
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: bounds,
                                  options: [.mouseEnteredAndExited, .activeAlways],
                                  owner: self)
        addTrackingArea(area)
        tracking = area
    }

    override func mouseEntered(with event: NSEvent) {
        hovering = true
        needsDisplay = true
    }

    override func mouseExited(with event: NSEvent) {
        hovering = false
        needsDisplay = true
    }

    override func mouseDown(with event: NSEvent) {
        pressPoint = superview?.convert(event.locationInWindow, from: nil)
        passedThreshold = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard let start = pressPoint,
              let point = superview?.convert(event.locationInWindow, from: nil)
        else { return }
        if !passedThreshold {
            guard abs(point.y - start.y) > dragThreshold else { return }
            passedThreshold = true
            delegate?.rowDidBeginDrag(self, grabOffset: start.y - frame.minY)
        }
        delegate?.rowDidDrag(self, to: point)
    }

    override func mouseUp(with event: NSEvent) {
        defer {
            pressPoint = nil
            passedThreshold = false
        }
        if passedThreshold {
            delegate?.rowDidEndDrag(self)
        } else if bounds.contains(convert(event.locationInWindow, from: nil)) {
            delegate?.rowWasClicked(self)
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        if isDragging {
            let lifted = bounds.insetBy(dx: Panel.gap, dy: 1)
            Panel.fill(lifted, radius: Panel.chipRadius, with: Panel.chipHover)
            Panel.accent.setStroke()
            let outline = NSBezierPath(roundedRect: lifted.insetBy(dx: 0.5, dy: 0.5),
                                       xRadius: Panel.chipRadius, yRadius: Panel.chipRadius)
            outline.lineWidth = 1
            outline.stroke()
        } else if hovering {
            Panel.fill(bounds.insetBy(dx: Panel.gap, dy: 1), radius: Panel.chipRadius,
                       with: Panel.rowHover)
        }
        if entry.isFront {
            // The window holding the stage, marked permanently — an affordance
            // the user can see without hovering to discover it.
            Panel.fill(NSRect(x: Panel.gap, y: 9, width: 3, height: bounds.height - 18),
                       radius: 1.5, with: Panel.accent)
        }

        let digit = NSRect(x: Panel.pad, y: 8, width: Panel.digitColumn, height: 14)
        Typeface.draw(entry.number.map { "⌘\($0)" } ?? " ·", Typeface.mono(12),
                      entry.number == nil ? Panel.faint : Panel.accent, in: digit)

        let iconX = Panel.pad + Panel.digitColumn + 6
        icon?.draw(in: NSRect(x: iconX, y: 7, width: Panel.icon, height: Panel.icon))

        let textX = iconX + Panel.icon + 8
        let textWidth = bounds.width - textX - Panel.pad
        Typeface.draw(appName, Typeface.mono(13, bold: true),
                      entry.isFront ? Panel.accent : Panel.text,
                      in: NSRect(x: textX, y: 5, width: textWidth, height: 16))
        Typeface.draw(entry.title, Typeface.mono(11), Panel.muted,
                      in: NSRect(x: textX, y: 22, width: textWidth, height: 14))
    }
}

// MARK: - Chip

/// A small pressable readout, shaped like the coordinate chip under the
/// magnifier: rounded, flat, one line of monospace.
private final class Chip: NSView {
    private let title: String
    private let isSelected: Bool
    private let onClick: () -> Void
    private var hovering = false
    private var tracking: NSTrackingArea?

    init(title: String, isSelected: Bool, onClick: @escaping () -> Void) {
        self.title = title
        self.isSelected = isSelected
        self.onClick = onClick
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var isFlipped: Bool { true }

    private var font: NSFont { Typeface.mono(11) }

    override var intrinsicContentSize: NSSize {
        NSSize(width: Typeface.width(title.uppercased(), font) + 22, height: Panel.chipHeight)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: bounds,
                                  options: [.mouseEnteredAndExited, .activeAlways],
                                  owner: self)
        addTrackingArea(area)
        tracking = area
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
        onClick()
    }

    override func draw(_ dirtyRect: NSRect) {
        let fill: NSColor = isSelected ? Panel.accent : (hovering ? Panel.chipHover : Panel.chip)
        Panel.fill(bounds, radius: Panel.chipRadius, with: fill)
        let text = title.uppercased()
        let width = Typeface.width(text, font)
        Typeface.draw(text, font, isSelected ? NSColor.black : Panel.text,
                      in: NSRect(x: (bounds.width - width) / 2, y: 6, width: width, height: 14))
    }
}
