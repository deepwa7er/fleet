import AppKit

/// Pure geometry for the solo-tile ring. Every window keeps the same size —
/// the screen minus a gap, like a tiling WM with a single window — so spins
/// are pure horizontal translation, the one thing AX does cheaply.
enum CarouselLayout {
    /// Margin around the stage, matching a tiling WM's outer gap.
    static var gap: CGFloat = 10

    /// The one frame every window keeps: screen inset by the gap.
    static func soloTile(screen s: CGRect) -> CGRect {
        s.insetBy(dx: gap, dy: gap)
    }

    /// Where a window at angle θ sits while the ring is in motion.
    enum Placement {
        /// On the filmstrip: horizontal offset from stage center. The strip's
        /// stride is one full screen width (tile + both gaps), so mid-scroll
        /// the outer gap glides between windows instead of showing a seam.
        case strip(offset: CGFloat)
        /// Beyond the strip. The window should hide dead-center behind the
        /// front window — the WindowServer clamps AX positions so ~40pt of a
        /// window always stays on-screen, making true off-screen parking
        /// impossible. `edgeOffset` is the strip position one stride out on
        /// the exit side, the fallback spot (clamped to a screen-edge sliver)
        /// for a window whose z-order doesn't allow hiding at center yet.
        case offStage(edgeOffset: CGFloat)
    }

    static func placement(atTheta theta: CGFloat, slotAngle: CGFloat, width: CGFloat) -> Placement {
        // Signed distance from the front of the ring, in slots.
        let twoPi = 2 * CGFloat.pi
        var wrapped = theta.truncatingRemainder(dividingBy: twoPi)
        if wrapped > .pi { wrapped -= twoPi }
        else if wrapped <= -.pi { wrapped += twoPi }
        let slots = wrapped / slotAngle
        let stride = width + 2 * gap
        if abs(slots) < 1 {
            return .strip(offset: slots * stride) // current and adjacent windows
        }
        return .offStage(edgeOffset: (slots > 0 ? 1 : -1) * stride)
    }

    /// 1 at the front (θ = 0), 0 at the back (θ = π); used only for z-order.
    static func depth(atTheta theta: CGFloat) -> CGFloat {
        (1 + cos(theta)) / 2
    }
}
