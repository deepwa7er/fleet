import CoreGraphics

/// Pure geometry for the stage: the one frame every managed window gets.
enum StageLayout {
    /// Margin around the stage, matching a tiling WM's outer gap. Equal on all
    /// four sides, so the stage is centred by construction.
    static var gap: CGFloat = 12

    /// The frame every window keeps: the display's visible area inset by the gap.
    ///
    /// Measured from the *visible* area, not the full display: the menu bar and
    /// Dock are not screen a window can occupy, and macOS clamps anything
    /// placed under them. On a display carrying neither — a second monitor,
    /// usually — the visible area is the whole screen and the gap is 12pt from
    /// the physical edge on every side.
    static func tile(screen: CGRect) -> CGRect {
        screen.insetBy(dx: gap, dy: gap)
    }
}
