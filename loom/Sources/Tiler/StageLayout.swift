import CoreGraphics

/// Pure geometry for the stage: the one frame every managed window gets.
enum StageLayout {
    /// Margin around the stage, matching a tiling WM's outer gap.
    static var gap: CGFloat = 10

    /// The frame every window keeps: the display's visible area inset by the gap.
    static func tile(screen: CGRect) -> CGRect {
        screen.insetBy(dx: gap, dy: gap)
    }
}
