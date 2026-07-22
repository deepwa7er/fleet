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

    /// Horizontal offset from stage center for a window at angle θ, used
    /// while the ring is in motion (at rest the carousel stacks everything
    /// behind the front window instead — the WindowServer clamps AX
    /// positions so a window can never sit fully off-screen).
    /// Windows form a filmstrip with a stride of one full screen width
    /// (tile + both gaps), so mid-scroll the outer gap glides between
    /// windows instead of showing a seam. Slots beyond the neighbors park
    /// off-stage (the back of the ring would otherwise sit centered behind
    /// the front window and get revealed mid-gesture); every park/unpark
    /// crossing happens off-screen, so nothing pops.
    static func xOffset(atTheta theta: CGFloat, slotAngle: CGFloat, width: CGFloat) -> CGFloat {
        // Signed distance from the front of the ring, in slots.
        let twoPi = 2 * CGFloat.pi
        var wrapped = theta.truncatingRemainder(dividingBy: twoPi)
        if wrapped > .pi { wrapped -= twoPi }
        else if wrapped <= -.pi { wrapped += twoPi }
        let slots = wrapped / slotAngle
        let stride = width + 2 * gap
        if abs(slots) <= 1 {
            return slots * stride // on the strip: current and adjacent windows
        }
        return (slots > 0 ? 1 : -1) * 2 * stride // parked off-stage
    }

    /// 1 at the front (θ = 0), 0 at the back (θ = π); used only for z-order.
    static func depth(atTheta theta: CGFloat) -> CGFloat {
        (1 + cos(theta)) / 2
    }
}
