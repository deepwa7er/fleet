import AppKit
import ScreenCaptureKit

/// One display as it looked the instant the overlay was armed.
struct FrozenDisplay {
    let screen: NSScreen
    let display: SCDisplay
    /// The whole display at native resolution, top-left origin like every
    /// other CoreGraphics image.
    let image: CGImage

    /// The display's AppKit global frame, in points.
    var frame: CGRect { screen.frame }
    var scale: CGFloat { CGFloat(image.width) / screen.frame.width }

    /// Crop out a region given in AppKit global points.
    ///
    /// Only the display-local flip is needed here — the region and the image
    /// belong to the same display, so the primary screen never enters into it.
    func crop(appKitRect rect: CGRect) -> CGImage? {
        let local = CGRect(x: rect.minX - frame.minX,
                           y: frame.maxY - rect.maxY,
                           width: rect.width, height: rect.height)
        let pixels = CGRect(x: local.minX * scale, y: local.minY * scale,
                            width: local.width * scale, height: local.height * scale)
        let bounds = CGRect(x: 0, y: 0, width: CGFloat(image.width), height: CGFloat(image.height))
        let clamped = pixels.intersection(bounds).integral
        guard !clamped.isNull, clamped.width >= 1, clamped.height >= 1 else { return nil }
        return image.cropping(to: clamped)
    }
}

/// Grabs every display up front, before the selection UI appears.
///
/// Capturing first and selecting afterwards is what makes the overlay honest:
/// the pixels the user drags a box around are the pixels they get, with no
/// second capture that could race a changing screen, and no need to hide the
/// overlay and hope the window server has caught up before the shutter fires.
/// It also means the magnifier has real pixels to zoom into.
enum ScreenFreezer {
    static func freeze(content: SCShareableContent) async throws -> [FrozenDisplay] {
        let ownWindows = ownWindows(in: content)
        var frozen: [FrozenDisplay] = []
        for display in content.displays {
            guard let screen = NSScreen.screen(withDisplayID: display.displayID) else {
                Log.error("no NSScreen matches display \(display.displayID); skipping it")
                continue
            }
            frozen.append(try await capture(display, screen: screen, excluding: ownWindows))
        }
        guard !frozen.isEmpty else { throw ShutterError.noDisplays }
        return frozen
    }

    /// The one display the pointer is on — what ⌘⇧3 grabs, with no overlay in
    /// the way, so there is no reason to pay for capturing the others.
    static func freezeDisplayUnderCursor() async throws -> FrozenDisplay {
        let content = try await SCShareableContent.excludingDesktopWindows(
            true, onScreenWindowsOnly: true)
        let ownWindows = ownWindows(in: content)
        let mouse = NSEvent.mouseLocation

        let candidates = content.displays.compactMap { display -> (SCDisplay, NSScreen)? in
            guard let screen = NSScreen.screen(withDisplayID: display.displayID) else { return nil }
            return (display, screen)
        }
        // Falling back to the first display rather than failing: the pointer
        // can sit in the seam between two screens of different heights.
        guard let target = candidates.first(where: { $0.1.frame.contains(mouse) })
                ?? candidates.first
        else { throw ShutterError.noDisplays }

        return try await capture(target.0, screen: target.1, excluding: ownWindows)
    }

    /// Shutter's own editor windows may be sitting on screen from an earlier
    /// capture. Excluding them shows what is behind them instead.
    private static func ownWindows(in content: SCShareableContent) -> [SCWindow] {
        let pid = ProcessInfo.processInfo.processIdentifier
        return content.windows.filter { $0.owningApplication?.processID == pid }
    }

    private static func capture(_ display: SCDisplay, screen: NSScreen,
                                excluding ownWindows: [SCWindow]) async throws -> FrozenDisplay {
        let scale = screen.backingScaleFactor
        let configuration = SCStreamConfiguration()
        configuration.width = Int((screen.frame.width * scale).rounded())
        configuration.height = Int((screen.frame.height * scale).rounded())
        configuration.captureResolution = .best
        configuration.showsCursor = false
        let filter = SCContentFilter(display: display, excludingWindows: ownWindows)
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter,
                                                               configuration: configuration)
        return FrozenDisplay(screen: screen, display: display, image: image)
    }
}
