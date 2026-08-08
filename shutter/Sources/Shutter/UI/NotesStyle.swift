import AppKit

/// The design language of the notes app, ported to AppKit.
///
/// Source of truth is that project's `docs/style-guide.md` and the rules
/// declared at the top of its `application.css`. The six rules, in short:
/// whitespace separates content; depth marks interactivity; warm light mode and
/// cool charcoal dark mode; one accent, on interactive elements only; metadata
/// is instrumentation; motion is engineered.
///
/// This applies to the editor window only. The selection overlay keeps its own
/// chrome in `Style` — it is drawn over a frozen screenshot rather than over a
/// page, so it has no background to be warm or cool against and has to stay
/// legible on top of arbitrary pixels.
///
/// The tokens are CSS custom properties, so they are `NSColor`s with dynamic
/// providers rather than fixed values: the stylesheet switches on
/// `prefers-color-scheme`, and the AppKit equivalent is resolving against the
/// view's effective appearance at draw time.
enum NotesStyle {
    // MARK: Tokens

    /// Page background. Warm cream paper / cool charcoal.
    static let bg = dynamic(light: 0xf7f2e9, dark: 0x121316)
    /// Raised surface: secondary and danger buttons.
    static let fill = dynamic(light: 0xfffdf8, dark: 0x1c1e22)
    static let text = dynamic(light: 0x1f1a12, dark: 0xeceef1)
    static let textMuted = dynamic(light: 0x7a7264, dark: 0x8b9199)
    /// The one accent. Interactive elements only.
    static let accent = dynamic(light: 0x0066b1, dark: 0x4d9de0)
    static let accentContrast = dynamic(light: 0xffffff, dark: 0x141311)
    static let danger = dynamic(light: 0xdc2626, dark: 0xf87171)
    /// Button outlines and hard shadows. Distinct from `text` on purpose: in
    /// dark mode ink goes to pure black so the offset shadow still reads
    /// against charcoal, while body text goes near-white.
    static let ink = dynamic(light: 0x1f1a12, dark: 0x000000)

    // MARK: Metrics

    /// `1rem`, the basis for everything the stylesheet expresses in rem.
    static let rem: CGFloat = 16

    static let cornerRadius: CGFloat = 10
    static let borderWidth: CGFloat = 2
    /// `box-shadow: 3px 3px 0` — solid and offset, never blurred.
    static let shadowOffset: CGFloat = 3

    /// `padding: 0.6rem 1.1rem`, which the stylesheet notes yields a ~44px
    /// touch target.
    static let buttonHeight: CGFloat = 44
    static let buttonPaddingH = 1.1 * rem
    /// `.actions { gap: 0.75rem }`
    static let actionGap = 0.75 * rem
    /// `.actions { margin-top: 1.5rem }`
    static let actionsTopMargin = 1.5 * rem
    /// `.container { padding: 1.5rem 1.25rem }`
    static let paddingH = 1.25 * rem
    static let paddingV = 1.5 * rem

    // MARK: Type

    /// `body { font-family: system-ui; font-size: 1rem }`
    static let body = NSFont.systemFont(ofSize: rem)
    /// Buttons are `font: inherit` at `font-weight: 600`.
    static let button = NSFont.systemFont(ofSize: rem, weight: .semibold)
    /// `.note-meta` — 0.7rem, 600, tabular numerals. Monospaced digits stop a
    /// live readout twitching as its digits change.
    static let meta = NSFont.monospacedDigitSystemFont(ofSize: 0.7 * rem, weight: .semibold)
    /// `.note-meta { letter-spacing: 0.08em }`
    static let metaTracking = 0.08 * 0.7 * rem

    /// Metadata is instrumentation: uppercase, letterspaced, muted.
    static func metaText(_ text: String, color: NSColor = NotesStyle.textMuted)
        -> NSAttributedString {
        NSAttributedString(string: text.uppercased(), attributes: [
            .font: meta,
            .foregroundColor: color,
            .kern: metaTracking,
        ])
    }

    // MARK: Motion

    /// "Short durations, strong ease-out, like a well-damped switch."
    static let motionDuration: CFTimeInterval = 0.15
    static let motionEasing = UnitBezier(0.2, 0.8, 0.2, 1)

    // MARK: Helpers

    /// CSS `filter: brightness(f)` multiplies each channel, which is a blend
    /// toward black by `1 - f`.
    static func brightness(_ color: NSColor, _ factor: CGFloat) -> NSColor {
        color.usingColorSpace(.sRGB)?.blended(withFraction: 1 - factor, of: .black) ?? color
    }

    private static func dynamic(light: UInt32, dark: UInt32) -> NSColor {
        let lightColor = srgb(light)
        let darkColor = srgb(dark)
        return NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? darkColor : lightColor
        }
    }

    private static func srgb(_ hex: UInt32) -> NSColor {
        NSColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255,
                green: CGFloat((hex >> 8) & 0xff) / 255,
                blue: CGFloat(hex & 0xff) / 255,
                alpha: 1)
    }
}

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function.
///
/// The curve is parametric, so a time fraction is not the curve parameter:
/// solve `x(t) = fraction` for `t` first, then evaluate `y(t)`.
struct UnitBezier {
    private let cx, bx, ax: Double
    private let cy, by, ay: Double

    init(_ x1: Double, _ y1: Double, _ x2: Double, _ y2: Double) {
        cx = 3 * x1
        bx = 3 * (x2 - x1) - cx
        ax = 1 - cx - bx
        cy = 3 * y1
        by = 3 * (y2 - y1) - cy
        ay = 1 - cy - by
    }

    private func x(_ t: Double) -> Double { ((ax * t + bx) * t + cx) * t }
    private func y(_ t: Double) -> Double { ((ay * t + by) * t + cy) * t }
    private func dx(_ t: Double) -> Double { (3 * ax * t + 2 * bx) * t + cx }

    func value(at fraction: Double) -> Double {
        y(solve(fraction))
    }

    private func solve(_ target: Double) -> Double {
        guard target > 0 else { return 0 }
        guard target < 1 else { return 1 }

        // Newton-Raphson converges in a couple of steps for these curves.
        var t = target
        for _ in 0..<8 {
            let error = x(t) - target
            if abs(error) < 1e-6 { return t }
            let slope = dx(t)
            if abs(slope) < 1e-6 { break }
            t -= error / slope
        }

        // Bisection, for the flat-slope case Newton cannot handle.
        var low = 0.0
        var high = 1.0
        t = target
        while low < high {
            let estimate = x(t)
            if abs(estimate - target) < 1e-6 { return t }
            if target > estimate { low = t } else { high = t }
            let next = (high - low) * 0.5 + low
            if abs(next - t) < 1e-9 { break }
            t = next
        }
        return t
    }
}
