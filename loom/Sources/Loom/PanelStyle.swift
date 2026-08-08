import AppKit

/// The house monospace, shared by every surface that shows window data.
///
/// Berkeley Mono is the face; the fallback is the system monospace rather than
/// the system sans, so a missing font degrades to the right *kind* of type
/// instead of to Helvetica.
enum Typeface {
    static func mono(_ size: CGFloat, bold: Bool = false) -> NSFont {
        NSFont(name: bold ? "BerkeleyMono-Bold" : "BerkeleyMono-Regular", size: size)
            ?? NSFont(name: bold ? "Berkeley Mono Bold" : "Berkeley Mono", size: size)
            ?? .monospacedSystemFont(ofSize: size, weight: bold ? .bold : .regular)
    }

    static func draw(_ text: String, _ font: NSFont, _ color: NSColor, in rect: NSRect) {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        NSAttributedString(string: text, attributes: [
            .font: font, .foregroundColor: color, .paragraphStyle: style,
        ]).draw(in: rect)
    }

    static func width(_ text: String, _ font: NSFont) -> CGFloat {
        ceil(NSAttributedString(string: text, attributes: [.font: font]).size().width)
    }
}

/// The command panel's palette, taken from the magnifier in Shutter's capture
/// overlay: a solid dark body with a crisp one-pixel light edge, flat fills,
/// and small readout chips. No blur, no soft shadow, no gradient — the whole
/// look is geometry and one hairline.
///
/// Nothing here follows the system appearance. The panel floats over arbitrary
/// application windows, so it has no background to be light or dark against and
/// has to bring its own contrast.
enum Panel {
    /// Near-opaque rather than translucent: the panel lands on top of whatever
    /// happens to be on screen, and window titles have to stay readable over it.
    static let body = NSColor(white: 0.04, alpha: 0.94)
    /// The magnifier's defining feature — a hairline bright enough to read as
    /// an instrument bezel.
    static let edge = NSColor(white: 1, alpha: 0.85)

    static let text = NSColor.white
    static let muted = NSColor(white: 1, alpha: 0.55)
    static let faint = NSColor(white: 1, alpha: 0.30)
    /// One signal colour, on the front window and the current selection only.
    static let accent = NSColor(srgbRed: 0.32, green: 0.72, blue: 1.0, alpha: 1)

    static let chip = NSColor(white: 1, alpha: 0.10)
    static let chipHover = NSColor(white: 1, alpha: 0.20)
    static let rowHover = NSColor(white: 1, alpha: 0.07)
    static let hairline = NSColor(white: 1, alpha: 0.12)

    static let radius: CGFloat = 12
    static let chipRadius: CGFloat = 6

    // Fixed metrics on a 4pt scale, so nothing in the panel floats off-grid.
    /// Opening width, before the user has resized it.
    static let width: CGFloat = 460
    static let minWidth: CGFloat = 360
    static let minHeight: CGFloat = 260
    /// How far into a corner counts as a grab for resizing. Kept clear of the
    /// rows and chips: the top corners land in the header, the bottom ones in
    /// the hint line, so a resize can never be mistaken for a click on either.
    static let cornerGrab: CGFloat = 16
    static let pad: CGFloat = 14
    static let row: CGFloat = 40
    static let chipHeight: CGFloat = 26
    static let gap: CGFloat = 6
    static let digitColumn: CGFloat = 30
    static let icon: CGFloat = 16

    static func fill(_ rect: NSRect, radius: CGFloat, with color: NSColor) {
        color.setFill()
        NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
    }
}

/// The size the user dragged the command panel to, remembered across openings.
///
/// Without this the panel would reopen at its content-fitted size every time and
/// resizing it would be pointless — the panel is summoned constantly, so the
/// size has to outlive a single showing.
enum PanelSize {
    private static let key = "CommandPanelSize"

    static var saved: NSSize? {
        get {
            guard let stored = UserDefaults.standard.dictionary(forKey: key) as? [String: CGFloat],
                  let width = stored["w"], let height = stored["h"] else { return nil }
            return NSSize(width: width, height: height)
        }
        set {
            guard let newValue else {
                UserDefaults.standard.removeObject(forKey: key)
                return
            }
            UserDefaults.standard.set(["w": newValue.width, "h": newValue.height], forKey: key)
        }
    }
}
