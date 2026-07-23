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

    /// The filmstrip's stride: one full screen width (tile + both gaps), so
    /// mid-scroll the outer gap glides between windows instead of a seam.
    static func stride(width: CGFloat) -> CGFloat {
        width + 2 * gap
    }

    /// Where a window at angle θ sits while the ring is in motion. Pure ring
    /// geometry — how an offset maps to an on-screen position depends on the
    /// display's surroundings and window identity, which Carousel owns.
    enum Placement {
        /// Within one stride of the front: offset from stage center.
        case strip(offset: CGFloat)
        /// Beyond the strip on the given side (-1 left, +1 right).
        case offStage(side: CGFloat)
    }

    static func placement(atTheta theta: CGFloat, slotAngle: CGFloat, width: CGFloat) -> Placement {
        // Signed distance from the front of the ring, in slots.
        let twoPi = 2 * CGFloat.pi
        var wrapped = theta.truncatingRemainder(dividingBy: twoPi)
        if wrapped > .pi { wrapped -= twoPi }
        else if wrapped <= -.pi { wrapped += twoPi }
        let slots = wrapped / slotAngle
        if abs(slots) < 1 {
            return .strip(offset: slots * stride(width: width))
        }
        return .offStage(side: slots > 0 ? 1 : -1)
    }

    /// 1 at the front (θ = 0), 0 at the back (θ = π); used only for z-order.
    static func depth(atTheta theta: CGFloat) -> CGFloat {
        (1 + cos(theta)) / 2
    }
}
