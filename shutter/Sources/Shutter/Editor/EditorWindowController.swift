import AppKit

/// The post-capture window: the image, the markup canvas, and the four things
/// you can do with the result.
///
/// Styled to the notes app's design language (see `NotesStyle`). That means one
/// surface the colour of paper, content separated by whitespace with no bars or
/// dividers, and depth reserved for the buttons. It also means the window
/// follows the system light/dark scheme rather than being fixed dark like the
/// selection overlay.
///
/// The buttons carry the key equivalents (⌘C, ⌘S, ⏎, esc) rather than a menu,
/// because an `LSUIElement` app never shows a menu bar to put them in. That
/// also makes the shortcuts discoverable instead of folklore.
@MainActor
final class EditorWindowController: NSWindowController, NSWindowDelegate {
    private let editor: EditorView
    private let onClose: (EditorWindowController) -> Void
    private let statusLabel = NSTextField(labelWithString: "")
    private var statusTimer: Timer?

    /// Room above the content for the traffic lights, which float over it now
    /// that the title bar is transparent.
    private static let titlebarInset: CGFloat = 28
    private static let metaGap: CGFloat = 12
    private static let hint = "Drag to draw · ⌫ delete · ⌘Z undo"

    init(capture: CapturedImage, onClose: @escaping (EditorWindowController) -> Void) {
        self.editor = EditorView(capture: capture)
        self.onClose = onClose

        let window = NSWindow(
            contentRect: CGRect(origin: .zero, size: Self.contentSize(for: capture)),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        // No title bar of its own: the page runs to the top edge and the
        // close/minimise buttons float over it.
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.backgroundColor = NotesStyle.bg
        window.isReleasedWhenClosed = false
        window.tabbingMode = .disallowed
        super.init(window: window)

        window.delegate = self
        window.contentView = buildContentView()
        window.center()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    deinit { statusTimer?.invalidate() }

    /// Fit the capture on screen at 1:1 where it will fit, shrunk to fit where
    /// it will not — a full-display capture is by definition as large as the
    /// screen it has to be shown on.
    private static func contentSize(for capture: CapturedImage) -> CGSize {
        let chrome = titlebarInset
            + NotesStyle.meta.pointSize + 4          // the metadata line
            + metaGap
            + NotesStyle.actionsTopMargin
            + NotesStyle.buttonHeight + NotesStyle.shadowOffset
            + NotesStyle.paddingV
        let available = NSScreen.main?.visibleFrame.size ?? CGSize(width: 1280, height: 800)
        let maximum = CGSize(width: available.width * 0.9 - NotesStyle.paddingH * 2,
                             height: available.height * 0.9 - chrome)
        let image = capture.pointSize
        let fit = min(1, min(maximum.width / max(image.width, 1),
                             maximum.height / max(image.height, 1)))
        return CGSize(width: max(image.width * fit + NotesStyle.paddingH * 2, 560),
                      height: image.height * fit + chrome)
    }

    private func buildContentView() -> NSView {
        let container = NSView()

        // Metadata is instrumentation (rule 5): the capture's pixel dimensions
        // on the left, the shortcut reminder and transient status on the right.
        let dimensions = NSTextField(labelWithString: "")
        dimensions.attributedStringValue = NotesStyle.metaText(
            String(format: "%.0f × %.0f",
                   editor.capture.pixelSize.width, editor.capture.pixelSize.height),
            color: NotesStyle.text)
        statusLabel.attributedStringValue = NotesStyle.metaText(Self.hint)

        let metaSpacer = NSView()
        metaSpacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let metaRow = NSStackView(views: [dimensions, metaSpacer, statusLabel])
        metaRow.orientation = .horizontal
        metaRow.alignment = .firstBaseline
        metaRow.spacing = NotesStyle.actionGap

        // `.actions`: a plain flex row on the page, left aligned. No bar, no
        // divider — whitespace is the separation (rule 1).
        let commit = PaperButton(title: "Save & Copy", variant: .primary, key: "\r",
                                 modifiers: [], target: self, action: #selector(saveAndCopy))
        let save = PaperButton(title: "Save", variant: .secondary, key: "s",
                               modifiers: .command, target: self, action: #selector(saveOnly))
        let copy = PaperButton(title: "Copy", variant: .secondary, key: "c",
                               modifiers: .command, target: self, action: #selector(copyOnly))
        let discard = PaperButton(title: "Discard", variant: .danger, key: "\u{1b}",
                                  modifiers: [], target: self, action: #selector(discard))

        let actionSpacer = NSView()
        actionSpacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let actions = NSStackView(views: [commit, save, copy, discard, actionSpacer])
        actions.orientation = .horizontal
        actions.alignment = .centerY
        actions.spacing = NotesStyle.actionGap
        actions.distribution = .fill

        for view in [container, metaRow, editor, actions] {
            view.translatesAutoresizingMaskIntoConstraints = false
        }
        container.addSubview(metaRow)
        container.addSubview(editor)
        container.addSubview(actions)

        let padding = NotesStyle.paddingH
        NSLayoutConstraint.activate([
            metaRow.topAnchor.constraint(equalTo: container.topAnchor,
                                         constant: Self.titlebarInset),
            metaRow.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: padding),
            metaRow.trailingAnchor.constraint(equalTo: container.trailingAnchor,
                                              constant: -padding),

            editor.topAnchor.constraint(equalTo: metaRow.bottomAnchor, constant: Self.metaGap),
            editor.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: padding),
            editor.trailingAnchor.constraint(equalTo: container.trailingAnchor,
                                             constant: -padding),
            editor.bottomAnchor.constraint(equalTo: actions.topAnchor,
                                           constant: -NotesStyle.actionsTopMargin),

            actions.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: padding),
            actions.trailingAnchor.constraint(equalTo: container.trailingAnchor,
                                              constant: -padding),
            actions.bottomAnchor.constraint(equalTo: container.bottomAnchor,
                                            constant: -NotesStyle.paddingV),
        ])
        return container
    }

    // MARK: Presenting

    func present() {
        NSApp.activate(ignoringOtherApps: true)
        window?.makeKeyAndOrderFront(nil)
        window?.makeFirstResponder(editor)
    }

    // MARK: Actions

    private func rendered() throws -> CGImage {
        try Renderer.compose(editor.capture, annotations: editor.annotations)
    }

    /// Confirmation goes to the metadata line rather than a floating chip —
    /// the same place the notes app puts its autosave readout.
    private func showStatus(_ message: String) {
        statusLabel.attributedStringValue = NotesStyle.metaText(message, color: NotesStyle.accent)
        statusTimer?.invalidate()
        statusTimer = Timer.scheduledTimer(withTimeInterval: 1.6, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.statusLabel.attributedStringValue = NotesStyle.metaText(Self.hint)
            }
        }
    }

    @objc private func copyOnly() {
        do {
            let image = try rendered()
            let pointSize = editor.capture.pointSize
            let file = try Destinations.copyFile(image, pointSize: pointSize)
            try Destinations.copyToClipboard(image, pointSize: pointSize, file: file)
            showStatus("Copied to clipboard")
        } catch {
            report(error)
        }
    }

    @objc private func saveOnly() {
        do {
            let url = try Destinations.save(rendered(), pointSize: editor.capture.pointSize)
            showStatus("Saved to \(url.deletingLastPathComponent().lastPathComponent)")
        } catch {
            report(error)
        }
    }

    @objc private func saveAndCopy() {
        do {
            let image = try rendered()
            let pointSize = editor.capture.pointSize
            let file = try Destinations.save(image, pointSize: pointSize)
            try Destinations.copyToClipboard(image, pointSize: pointSize, file: file)
            close()
        } catch {
            report(error)
        }
    }

    @objc private func discard() {
        close()
    }

    private func report(_ error: Error) {
        Log.error("\(error)")
        let alert = NSAlert(error: error)
        alert.runModal()
    }

    func windowWillClose(_ notification: Notification) {
        onClose(self)
    }
}
