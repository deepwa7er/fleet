import AppKit

/// The selection overlay's visual language: flat dark chrome, white text,
/// rounded pills.
///
/// Nothing here reads from the system appearance or the user's accent colour,
/// and that is the point: the overlay is drawn over a frozen screenshot rather
/// than over a window background, so it has no appearance to inherit and has to
/// stay legible on top of arbitrary pixels.
///
/// The editor window is styled separately, from the notes app's design language
/// — see `NotesStyle`. The two surfaces are deliberately not the same.
enum Style {
    /// Chrome floating over a screenshot, where it has to stay legible on top
    /// of whatever happens to be behind it.
    static let translucentChrome = NSColor(white: 0, alpha: 0.75)
    static let text = NSColor.white

    /// Marks the window or display under the pointer. Deliberately not
    /// `controlAccentColor`: the overlay should look the same on every machine.
    static let highlight = NSColor(srgbRed: 0.32, green: 0.72, blue: 1.0, alpha: 1)

    static let cornerRadius: CGFloat = 6

    /// Monospaced digits keep a live size readout from twitching as it counts.
    static let label = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .medium)

    private static let pillPadding = CGSize(width: 10, height: 6)

    static func attributed(_ text: String, color: NSColor = Style.text,
                           font: NSFont = Style.label) -> NSAttributedString {
        NSAttributedString(string: text, attributes: [.font: font, .foregroundColor: color])
    }

    static func pillSize(for text: String, font: NSFont = Style.label) -> CGSize {
        let size = attributed(text, font: font).size()
        return CGSize(width: ceil(size.width) + pillPadding.width * 2,
                      height: ceil(size.height) + pillPadding.height * 2)
    }

    /// Draws a rounded dark chip with `text` in it, anchored at its lower-left.
    static func drawPill(_ text: String, at origin: CGPoint, in ctx: CGContext,
                         font: NSFont = Style.label) {
        let rect = CGRect(origin: origin, size: pillSize(for: text, font: font))
        fill(roundedRect: rect, with: translucentChrome, in: ctx)
        attributed(text, font: font).draw(at: CGPoint(x: rect.minX + pillPadding.width,
                                                      y: rect.minY + pillPadding.height))
    }

    static func fill(roundedRect rect: CGRect, with color: NSColor, in ctx: CGContext) {
        ctx.saveGState()
        ctx.setFillColor(color.cgColor)
        ctx.addPath(CGPath(roundedRect: rect, cornerWidth: cornerRadius,
                           cornerHeight: cornerRadius, transform: nil))
        ctx.fillPath()
        ctx.restoreGState()
    }
}
