import AppKit

/// macOS hands out two global coordinate spaces that disagree about which way y
/// runs: AppKit's (origin at the bottom-left of the primary screen, y up) and
/// CoreGraphics'/ScreenCaptureKit's (origin at the top-left, y down). Both are
/// anchored to the primary screen, so its height is the flip axis.
///
/// Every conversion between a mouse location and a capture rect goes through
/// here rather than being open-coded at the call site, because a flip applied
/// twice — or not at all — produces a screenshot of the wrong part of the
/// screen that still looks plausible.
enum Geometry {
    /// The screen AppKit places at the origin, which is also CoreGraphics'
    /// top-left anchor. `NSScreen.screens` is documented to lead with it.
    static var primaryHeight: CGFloat {
        NSScreen.screens.first?.frame.height ?? 0
    }

    static func appKitRect(fromCG rect: CGRect) -> CGRect {
        CGRect(x: rect.minX, y: primaryHeight - rect.maxY, width: rect.width, height: rect.height)
    }

    static func cgRect(fromAppKit rect: CGRect) -> CGRect {
        // The flip is its own inverse.
        appKitRect(fromCG: rect)
    }
}

extension NSScreen {
    /// The `CGDirectDisplayID` this screen renders on, used to line NSScreen up
    /// with the `SCDisplay` describing the same physical monitor.
    var displayID: CGDirectDisplayID? {
        (deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value
    }

    static func screen(withDisplayID id: CGDirectDisplayID) -> NSScreen? {
        screens.first { $0.displayID == id }
    }

    /// The screen showing most of `rect` (AppKit global points), which is the
    /// one whose backing scale a capture of that rect should use.
    static func screen(bestMatching rect: CGRect) -> NSScreen? {
        screens.max { a, b in
            a.frame.intersection(rect).area < b.frame.intersection(rect).area
        }
    }
}

extension CGRect {
    var area: CGFloat { isNull ? 0 : width * height }

    /// The rect two corners describe, whichever order they were dragged in.
    init(corner a: CGPoint, corner b: CGPoint) {
        self.init(x: min(a.x, b.x), y: min(a.y, b.y),
                  width: abs(a.x - b.x), height: abs(a.y - b.y))
    }
}
