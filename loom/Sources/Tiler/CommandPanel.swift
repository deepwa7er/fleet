import AppKit
import Filament
import FilamentAppKit

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
///
/// It stays up until it is explicitly toggled away — clicking into another app
/// does not dismiss it. Sitting at `.popUpMenu` level, above every ordinary
/// window, that means it keeps the top of the stack while windows are picked,
/// focused and worked with underneath it. The only ways out are the microphone
/// key, Escape, and the ◎ menu item.
final class CommandPanel {
    private let stack: WindowStack
    private var panel: NSPanel?
    /// Keeps the front marker current while the panel stays open. `focus` is an
    /// AX raise plus an app activation, both asynchronous, so the WindowServer's
    /// idea of the front window lags the click that caused it — there is nothing
    /// to observe synchronously, and a fixed delay would just be a guess.
    private var frontTimer: Timer?

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
                // Deliberately no dismiss: the panel stays put so windows can
                // be picked one after another. It sits at `.popUpMenu` level,
                // above every ordinary window, so raising the chosen window
                // brings it to the top of the normal level — directly beneath
                // the panel rather than over it.
                self?.stack.switchToWindow(id: id)
            },
            onClose: { [weak self] id in
                self?.stack.closeWindow(id: id)
                // The row goes when the stack's next reconcile drops the window;
                // the poll below notices and rebuilds.
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
            onRetile: { [weak self] in
                // Stays open: the panel floats above every ordinary window, so
                // the whole stack snapping onto the stage is visible from here.
                self?.stack.retileAll()
            },
            onLoginToggle: { [weak self] in
                self?.toggleStartAtLogin()
                self?.reload()
            },
            onQuit: { NSApp.terminate(nil) },
            onSend: { [weak self] id, uuid in
                self?.stack.sendWindow(id: id, toDisplay: uuid)
                // The window leaves the managed display, so the next membership
                // poll drops its row on its own.
                self?.reload()
            })

        let anchor = NSEvent.mouseLocation
        let screen = NSScreen.screens.first { $0.frame.contains(anchor) } ?? NSScreen.main
        let available = screen?.visibleFrame.size ?? NSSize(width: 1440, height: 900)
        content.setFrameSize(NSSize(width: Panel.width, height: Panel.minHeight))
        content.reload()
        // The size the user dragged to wins; without one, fit the content.
        let wanted = PanelSize.saved ?? content.naturalSize(maxHeight: available.height * 0.75)
        content.setFrameSize(NSSize(width: min(wanted.width, available.width - 16),
                                    height: min(wanted.height, available.height - 16)))

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

        frontTimer = Timer.scheduledTimer(withTimeInterval: 0.4, repeats: true) { [weak self] _ in
            guard let self, let content = self.panel?.contentView as? CommandContentView
            else { return }
            // Membership first. The panel stays open now, so windows closing or
            // opening underneath it have to appear without being summoned again.
            if content.membership == self.stack.windowIDs() {
                content.updateFront(self.stack.frontWindowID())
            } else {
                content.reload()
            }
        }
    }

    func dismiss() {
        frontTimer?.invalidate()
        frontTimer = nil
        panel?.orderOut(nil)
        panel = nil
    }

    /// Rebuild in place after an action that changes what the panel shows,
    /// keeping it where the user put it rather than re-anchoring to a pointer
    /// that has since moved.
    private func reload() {
        guard let panel, let content = panel.contentView as? CommandContentView else { return }
        // The window keeps whatever size it has been dragged to, so a data
        // change rebuilds the content in place rather than resizing around it.
        content.reload()
        panel.display()
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
    private let onRetile: () -> Void
    private let onLoginToggle: () -> Void
    private let onQuit: () -> Void
    private let onSend: (CGWindowID, String) -> Void
    /// Set by right-clicking a row: the display section turns into a picker for
    /// where to send that window, and turns back once one is chosen.
    private var sendTarget: (id: CGWindowID, name: String)?

    private let scrollView = NSScrollView()
    private let list: WindowListView

    /// The chips live inside these rather than directly in the panel, so both
    /// the legacy and the Filament path put them in the same place and layout
    /// has only one implementation to care about. It also keeps the
    /// reconciler's containers free of views it did not create, which is the
    /// one invariant an AppKit host depends on.
    private let displayChipsHost = FlippedView()
    private let actionChipsHost = FlippedView()
    private var displayChipsRenderer: ChipRowRenderer?
    private var actionChipsRenderer: ChipRowRenderer?

    private var entries: [WindowStack.WindowEntry] = []
    private var displayName = ""

    /// Where the sections landed in the last layout, so `draw` can put the
    /// labels and rules in the same places without recomputing them.
    private var listFrame = NSRect.zero
    private var displaysLabelY: CGFloat = 0
    private var actionsLabelY: CGFloat = 0
    private var hintY: CGFloat = 0
    private var lastCursorInvalidationSize: NSSize = .zero

    init(stack: WindowStack,
         onPick: @escaping (CGWindowID) -> Void,
         onClose: @escaping (CGWindowID) -> Void,
         onReorder: @escaping ([CGWindowID]) -> Void,
         onDisplay: @escaping (String) -> Void,
         onRetile: @escaping () -> Void,
         onLoginToggle: @escaping () -> Void,
         onQuit: @escaping () -> Void,
         onSend: @escaping (CGWindowID, String) -> Void) {
        self.stack = stack
        self.onPick = onPick
        self.list = WindowListView(onPick: onPick, onClose: onClose, onReorder: onReorder)
        self.onDisplay = onDisplay
        self.onRetile = onRetile
        self.onLoginToggle = onLoginToggle
        self.onQuit = onQuit
        self.onSend = onSend
        super.init(frame: .zero)
        list.onSendRequest = { [weak self] id, name in
            self?.sendTarget = (id, name)
            self?.reload()
        }
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.documentView = list
        addSubview(scrollView)
        addSubview(displayChipsHost)
        addSubview(actionChipsHost)

        if FeatureFlags.filamentChips {
            displayChipsRenderer = ChipRowRenderer(container: displayChipsHost)
            actionChipsRenderer = ChipRowRenderer(container: actionChipsHost)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var isFlipped: Bool { true }

    // MARK: Moving and resizing

    private enum Corner { case topLeft, topRight, bottomLeft, bottomRight }
    private var resizing: Corner?
    /// The corner diagonally opposite the one being dragged. It stays put while
    /// the dragged corner follows the pointer, which is what makes a resize feel
    /// like a resize rather than a move.
    private var anchor = CGPoint.zero

    private func corner(at point: CGPoint) -> Corner? {
        let grab = Panel.cornerGrab
        let left = point.x <= grab, right = point.x >= bounds.width - grab
        let top = point.y <= grab, bottom = point.y >= bounds.height - grab
        if top, left { return .topLeft }
        if top, right { return .topRight }
        if bottom, left { return .bottomLeft }
        if bottom, right { return .bottomRight }
        return nil
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    /// A corner drag resizes; anywhere else that isn't a row or a chip moves.
    ///
    /// A borderless window has no title bar to grab and no resize control, so
    /// the body has to be both handles. `mouseDown` only reaches this view when
    /// no subview claimed the event, which is exactly the split we want: rows
    /// keep their own drag-to-reorder and chips keep their clicks.
    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        guard let grabbed = corner(at: point), let window else {
            window?.performDrag(with: event)
            return
        }
        resizing = grabbed
        // Window frames are bottom-left origin; this view is flipped, so its
        // "top" corners are the frame's high-y ones.
        let frame = window.frame
        switch grabbed {
        case .topLeft: anchor = CGPoint(x: frame.maxX, y: frame.minY)
        case .topRight: anchor = CGPoint(x: frame.minX, y: frame.minY)
        case .bottomLeft: anchor = CGPoint(x: frame.maxX, y: frame.maxY)
        case .bottomRight: anchor = CGPoint(x: frame.minX, y: frame.maxY)
        }
    }

    override func mouseDragged(with event: NSEvent) {
        guard resizing != nil, let window else { return }
        let mouse = NSEvent.mouseLocation
        let width = max(abs(mouse.x - anchor.x), Panel.minWidth)
        let height = max(abs(mouse.y - anchor.y), Panel.minHeight)
        // Clamping the size must not drag the anchored corner along with it.
        window.setFrame(NSRect(x: mouse.x < anchor.x ? anchor.x - width : anchor.x,
                               y: mouse.y < anchor.y ? anchor.y - height : anchor.y,
                               width: width, height: height),
                        display: true)
    }

    override func mouseUp(with event: NSEvent) {
        guard resizing != nil else { return }
        resizing = nil
        PanelSize.saved = bounds.size
    }

    override func resetCursorRects() {
        let grab = Panel.cornerGrab
        for rect in [NSRect(x: 0, y: 0, width: grab, height: grab),
                     NSRect(x: bounds.width - grab, y: 0, width: grab, height: grab),
                     NSRect(x: 0, y: bounds.height - grab, width: grab, height: grab),
                     NSRect(x: bounds.width - grab, y: bounds.height - grab,
                            width: grab, height: grab)] {
            addCursorRect(rect, cursor: .crosshair)
        }
    }

    // MARK: Layout

    private let labelHeight: CGFloat = 16
    private let hintHeight: CGFloat = 14

    private var headerHeight: CGFloat { Panel.pad + labelHeight + Panel.gap }

    /// Read the stack and rebuild the rows and chips.
    ///
    /// Kept apart from `layOut` because reading the stack costs an accessibility
    /// round trip per window for its title. That is fine when the panel opens.
    /// It is not fine sixty times a second while a corner is being dragged.
    func reload() {
        entries = stack.windowList()
        displayName = Displays.screen(matching: stack.selectedUUID)?.localizedName ?? "—"

        populate(displayChipsHost, with: displayChipSpecs(), using: displayChipsRenderer)
        populate(actionChipsHost, with: actionChipSpecs(), using: actionChipsRenderer)

        list.setEntries(entries)
        layOut()
    }

    /// The display picker: every connected screen as a chip, so there is no
    /// submenu anywhere in the panel.
    private func displayChipSpecs() -> [ChipSpec] {
        let selected = Displays.screen(matching: stack.selectedUUID).flatMap(Displays.uuid(of:))

        guard let target = sendTarget else {
            return NSScreen.screens.compactMap { screen in
                guard let uuid = Displays.uuid(of: screen) else { return nil }
                return ChipSpec(title: screen.localizedName, isSelected: uuid == selected) {
                    [weak self] in self?.onDisplay(uuid)
                }
            }
        }

        // Only the displays it could go to: sending a window to the one it is
        // already on is a no-op dressed up as a choice.
        var specs = NSScreen.screens.compactMap { screen -> ChipSpec? in
            guard let uuid = Displays.uuid(of: screen), uuid != selected else { return nil }
            return ChipSpec(title: screen.localizedName, isSelected: false) { [weak self] in
                self?.sendTarget = nil
                self?.onSend(target.id, uuid)
            }
        }
        specs.append(ChipSpec(title: "Cancel", isSelected: false) { [weak self] in
            self?.sendTarget = nil
            self?.reload()
        })
        return specs
    }

    private func actionChipSpecs() -> [ChipSpec] {
        let loginTitle: String
        switch LoginItem.status {
        case .requiresApproval: loginTitle = "Login: approve in Settings"
        case .enabled: loginTitle = "Start at login: on"
        default: loginTitle = "Start at login: off"
        }
        return [
            ChipSpec(title: "Retile all", isSelected: false, onClick: onRetile),
            ChipSpec(title: loginTitle, isSelected: LoginItem.isEnabled, onClick: onLoginToggle),
            ChipSpec(title: "Quit", isSelected: false, onClick: onQuit),
        ]
    }

    /// Makes a container's chips match `specs`.
    ///
    /// Both paths are handed the same specs and fill the same container, so the
    /// only thing under comparison is how they get there. The legacy path
    /// discards every chip and builds new ones; the reconciler updates the
    /// chips that changed and leaves the rest alone.
    private func populate(_ container: NSView, with specs: [ChipSpec], using renderer: ChipRowRenderer?) {
        if let renderer {
            renderer.render(specs)
            return
        }
        container.subviews.forEach { $0.removeFromSuperview() }
        for spec in specs {
            container.addSubview(
                Chip(title: spec.title, isSelected: spec.isSelected, onClick: spec.onClick)
            )
        }
    }

    /// The window ids currently on show, for the cheap membership check.
    var membership: [CGWindowID] { entries.map(\.id) }

    /// The size the panel would like: tall enough for the whole stack, capped so
    /// a long one scrolls rather than running off the screen.
    func naturalSize(maxHeight: CGFloat) -> NSSize {
        let width = max(bounds.width, Panel.width)
        let below = bottomHeight(width: width)
        let wanted = CGFloat(max(entries.count, 1)) * Panel.row
        let room = max(Panel.row * 2, maxHeight - headerHeight - below)
        return NSSize(width: width, height: headerHeight + min(wanted, room) + below)
    }

    /// Everything below the window list. Its height depends on how the chips
    /// wrap, which depends on the width — so it cannot be the constant it was
    /// when the panel had only one width.
    private func bottomHeight(width: CGFloat) -> CGFloat {
        let inner = width - Panel.pad * 2
        return Panel.pad
            + labelHeight + Panel.gap + chipBlock(displayChipsHost, width: inner) + Panel.pad
            + labelHeight + Panel.gap + chipBlock(actionChipsHost, width: inner) + Panel.pad
            + hintHeight + Panel.pad
    }

    private func chipBlock(_ container: NSView, width: CGFloat) -> CGFloat {
        let chips = container.subviews
        guard !chips.isEmpty else { return 0 }
        var rows = 1
        var x: CGFloat = 0
        for chip in chips {
            let chipWidth = chip.intrinsicContentSize.width
            if x > 0, x + chipWidth > width {
                rows += 1
                x = 0
            }
            x += chipWidth + Panel.gap
        }
        return CGFloat(rows) * Panel.chipHeight + CGFloat(rows - 1) * Panel.gap
    }

    /// Position everything from the current bounds. The window owns the size
    /// now, and the window list absorbs whatever height is left over.
    func layOut() {
        let width = bounds.width
        let inner = width - Panel.pad * 2

        let visible = max(Panel.row, bounds.height - headerHeight - bottomHeight(width: width))
        listFrame = NSRect(x: 0, y: headerHeight, width: width, height: visible)
        scrollView.frame = listFrame
        list.frame = NSRect(x: 0, y: 0, width: width,
                            height: CGFloat(max(entries.count, 1)) * Panel.row)
        list.relayout()

        var y = headerHeight + visible + Panel.pad
        displaysLabelY = y
        y += labelHeight + Panel.gap
        y = place(displayChipsHost, from: y, width: inner)
        y += Panel.pad
        actionsLabelY = y
        y += labelHeight + Panel.gap
        y = place(actionChipsHost, from: y, width: inner)
        y += Panel.pad
        hintY = y

        if bounds.size != lastCursorInvalidationSize {
            window?.invalidateCursorRects(for: self)
            lastCursorInvalidationSize = bounds.size
        }
        needsDisplay = true
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        layOut()
    }

    /// Move the front marker without rebuilding anything — a relayout here
    /// would fight an in-progress drag and reset every hover state.
    func updateFront(_ id: CGWindowID?) {
        list.updateFront(id)
    }

    /// Lay a container's chips left to right, wrapping when the row is full.
    /// Returns the panel-relative y just past the last row.
    ///
    /// Chip frames are container-relative now, so the container is sized to
    /// whatever its chips actually span rather than to the row width. A chip
    /// wider than the row would otherwise sit outside its parent's bounds,
    /// where AppKit stops sending it mouse events — it would still draw, and
    /// silently stop being clickable.
    private func place(_ container: NSView, from top: CGFloat, width: CGFloat) -> CGFloat {
        var x: CGFloat = 0
        var y: CGFloat = 0
        var right: CGFloat = 0

        for chip in container.subviews {
            let size = chip.intrinsicContentSize
            if x > 0, x + size.width > width {
                x = 0
                y += Panel.chipHeight + Panel.gap
            }
            chip.frame = NSRect(x: x, y: y, width: size.width, height: Panel.chipHeight)
            right = max(right, x + size.width)
            x += size.width + Panel.gap
        }

        let height = container.subviews.isEmpty ? 0 : y + Panel.chipHeight
        container.frame = NSRect(
            x: Panel.pad, y: top, width: max(width, right), height: height
        )
        return top + height
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

        label(sendTarget.map { "SEND \($0.name.uppercased()) TO" } ?? "DISPLAY",
              at: displaysLabelY)
        label("ACTIONS", at: actionsLabelY)

        Typeface.draw("CLICK FOCUS · RIGHT-CLICK SEND · DRAG REORDER · ✕ CLOSE",
                      Typeface.mono(10), Panel.faint,
                      in: NSRect(x: Panel.pad, y: hintY,
                                 width: bounds.width - Panel.pad * 2, height: 14))

        drawCornerGrips()

        // The bezel, drawn last so nothing overlaps it. Inset by half a point
        // so the one-pixel stroke lands on the pixel instead of straddling it.
        Panel.edge.setStroke()
        let bezel = NSBezierPath(roundedRect: body, xRadius: Panel.radius, yRadius: Panel.radius)
        bezel.lineWidth = 1
        bezel.stroke()
    }

    /// Short marks inside each corner. A borderless window shows no resize
    /// control, so without them the corners are a secret.
    private func drawCornerGrips() {
        let inset: CGFloat = 7, arm: CGFloat = 6
        let path = NSBezierPath()
        for (x, y, dx, dy) in [(inset, inset, 1.0, 1.0),
                               (bounds.maxX - inset, inset, -1.0, 1.0),
                               (inset, bounds.maxY - inset, 1.0, -1.0),
                               (bounds.maxX - inset, bounds.maxY - inset, -1.0, -1.0)] {
            path.move(to: NSPoint(x: x, y: y + arm * dy))
            path.line(to: NSPoint(x: x, y: y))
            path.line(to: NSPoint(x: x + arm * dx, y: y))
        }
        Panel.faint.setStroke()
        path.lineWidth = 1
        path.stroke()
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
    private let onClose: (CGWindowID) -> Void
    /// Set after init: the content view handles this and cannot pass a closure
    /// capturing itself while it is still building its own subviews.
    var onSendRequest: ((CGWindowID, String) -> Void)?
    private let onReorder: ([CGWindowID]) -> Void
    private var rows: [WindowRow] = []

    private var dragged: WindowRow?
    /// Where inside the row the pointer took hold, so it doesn't jump to centre
    /// itself under the cursor.
    private var grab: CGFloat = 0
    private var target = 0

    init(onPick: @escaping (CGWindowID) -> Void,
         onClose: @escaping (CGWindowID) -> Void,
         onReorder: @escaping ([CGWindowID]) -> Void) {
        self.onPick = onPick
        self.onClose = onClose
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

    func updateFront(_ id: CGWindowID?) {
        for row in rows { row.isFront = row.windowID == id }
    }

    /// Reposition the existing rows for a new width, without rebuilding them.
    func relayout() { layoutRows() }

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

    func rowWasClosed(_ row: WindowRow) {
        onClose(row.windowID)
    }

    func rowRequestedSend(_ row: WindowRow) {
        onSendRequest?(row.windowID, row.appName)
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
    func rowWasClosed(_ row: WindowRow)
    func rowRequestedSend(_ row: WindowRow)
    func rowDidBeginDrag(_ row: WindowRow, grabOffset: CGFloat)
    /// `point` is in the list's coordinates — the row itself is moving, so its
    /// own coordinate space is not a fixed reference during a drag.
    func rowDidDrag(_ row: WindowRow, to point: CGPoint)
    func rowDidEndDrag(_ row: WindowRow)
}

private final class WindowRow: NSView {
    let windowID: CGWindowID
    let appName: String
    weak var delegate: (any WindowRowDelegate)?
    var isDragging = false { didSet { needsDisplay = true } }
    var isFront: Bool { didSet { if isFront != oldValue { needsDisplay = true } } }

    private let entry: WindowStack.WindowEntry
    private let icon: NSImage?
    private var hovering = false
    private var tracking: NSTrackingArea?
    private var lastTrackingRect: NSRect = .zero
    private var pressPoint: CGPoint?
    private var passedThreshold = false
    private var pressingClose = false

    /// The ✕ target, at the trailing edge. Its width is reserved in the title
    /// column at all times, so revealing it on hover never reflows the text.
    private var closeRect: CGRect {
        CGRect(x: bounds.maxX - Panel.pad - Panel.icon,
               y: (bounds.height - Panel.icon) / 2,
               width: Panel.icon, height: Panel.icon)
    }

    /// How far the pointer must travel before a click becomes a drag.
    private let dragThreshold: CGFloat = 4

    init(entry: WindowStack.WindowEntry) {
        self.entry = entry
        self.windowID = entry.id
        self.isFront = entry.isFront
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

    override func rightMouseDown(with event: NSEvent) {
        delegate?.rowRequestedSend(self)
    }

    override func mouseDown(with event: NSEvent) {
        if closeRect.contains(convert(event.locationInWindow, from: nil)) {
            pressingClose = true
            return
        }
        pressPoint = superview?.convert(event.locationInWindow, from: nil)
        passedThreshold = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard !pressingClose, let start = pressPoint,
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
        if pressingClose {
            pressingClose = false
            // Only a press that both started and ended on the ✕ closes, so a
            // press dragged off it is a cancel, as anywhere else on macOS.
            if closeRect.contains(convert(event.locationInWindow, from: nil)) {
                delegate?.rowWasClosed(self)
            }
            return
        }
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
        if isFront {
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
        let textWidth = bounds.width - textX - Panel.pad - Panel.icon - 8
        Typeface.draw(appName, Typeface.mono(13, bold: true),
                      isFront ? Panel.accent : Panel.text,
                      in: NSRect(x: textX, y: 5, width: textWidth, height: 16))
        Typeface.draw(entry.title, Typeface.mono(11), Panel.muted,
                      in: NSRect(x: textX, y: 22, width: textWidth, height: 14))

        // The ✕ appears on hover: eight of these stacked would otherwise be a
        // column of close buttons inviting a misclick.
        if hovering {
            let box = closeRect.insetBy(dx: 4, dy: 4)
            let mark = NSBezierPath()
            mark.move(to: NSPoint(x: box.minX, y: box.minY))
            mark.line(to: NSPoint(x: box.maxX, y: box.maxY))
            mark.move(to: NSPoint(x: box.maxX, y: box.minY))
            mark.line(to: NSPoint(x: box.minX, y: box.maxY))
            Panel.muted.setStroke()
            mark.lineWidth = 1.5
            mark.stroke()
        }
    }
}

// MARK: - Chip

/// A small pressable readout, shaped like the coordinate chip under the
/// magnifier: rounded, flat, one line of monospace.
final class Chip: NSView, PropApplying {
    // Mutable so a chip can be updated in place. Nothing about a chip's
    // appearance depends on which object it is, so recreating one to change its
    // title was always work for its own sake.
    private var title: String
    private var isSelected: Bool
    private var onClick: () -> Void
    private var hovering = false
    private var tracking: NSTrackingArea?
    private var lastTrackingRect: NSRect = .zero

    init(title: String, isSelected: Bool, onClick: @escaping () -> Void) {
        self.title = title
        self.isSelected = isSelected
        self.onClick = onClick
        super.init(frame: .zero)
    }

    /// Builds a chip from an element's props, for the reconciler.
    convenience init(props: Props) {
        self.init(title: "", isSelected: false, onClick: {})
        applyProps(updated: props.storage, removed: [])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    /// Applies only what changed. A handler can never compare equal, so
    /// `onClick` is re-bound on every render — a closure assignment, against
    /// the cost of building a whole new view, which is what the other path does.
    func applyProps(updated: [String: Props.Value], removed: [String]) {
        var needsRedraw = false

        for (key, value) in updated {
            switch (key, value) {
            case ("title", .string(let text)) where text != title:
                title = text
                invalidateIntrinsicContentSize()
                needsRedraw = true
            case ("selected", .bool(let flag)) where flag != isSelected:
                isSelected = flag
                needsRedraw = true
            case ("onClick", .handler(let action)):
                onClick = action
            default:
                break
            }
        }

        for key in removed where key == "selected" {
            isSelected = false
            needsRedraw = true
        }

        if needsRedraw { needsDisplay = true }
    }

    override var isFlipped: Bool { true }

    private var font: NSFont { Typeface.mono(11) }

    override var intrinsicContentSize: NSSize {
        NSSize(width: Typeface.width(title.uppercased(), font) + 22, height: Panel.chipHeight)
    }

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
