import AppKit

/// Frame-synced driver: while running, calls the tick once per display
/// refresh on the main run loop, passing the frame timestamp (in the
/// `CACurrentMediaTime` timebase). The tick returns whether animation is
/// still running; the link tears down when it returns false, so it only
/// exists — and only retains us — while something is actually moving.
final class Animator {
    private var link: CADisplayLink?
    private let tick: (CFTimeInterval) -> Bool

    var isAnimating: Bool { link != nil }

    init(tick: @escaping (CFTimeInterval) -> Bool) {
        self.tick = tick
    }

    func start() {
        guard link == nil, let screen = NSScreen.screens.first else { return }
        let link = screen.displayLink(target: self, selector: #selector(step(_:)))
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    @objc private func step(_ sender: CADisplayLink) {
        guard !tick(sender.timestamp) else { return }
        sender.invalidate()
        link = nil
    }
}
