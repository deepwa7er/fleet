import AppKit
import ScreenCaptureKit

enum WindowCapturer {
    /// Capture one window by itself.
    ///
    /// This is the one mode that does *not* crop the frozen screen. A window
    /// can be partly covered, and cropping the frozen image to its frame would
    /// bake whatever sits on top of it into the shot. A desktop-independent
    /// filter renders the window alone, unoccluded, with a transparent
    /// background around its rounded corners.
    ///
    /// The cost is that the pixels come from the moment of the click rather
    /// than the moment the overlay froze, so a window playing video may hand
    /// back a slightly later frame than the one that was highlighted. That is
    /// the better trade: a fresh frame of the right window beats a stale frame
    /// with someone else's window sitting across it.
    static func capture(_ window: SCWindow) async throws -> CapturedImage {
        let frame = Geometry.appKitRect(fromCG: window.frame)
        let scale = NSScreen.screen(bestMatching: frame)?.backingScaleFactor
            ?? NSScreen.main?.backingScaleFactor ?? 2

        let configuration = SCStreamConfiguration()
        configuration.width = Int((frame.width * scale).rounded())
        configuration.height = Int((frame.height * scale).rounded())
        configuration.captureResolution = .best
        configuration.showsCursor = false
        configuration.backgroundColor = .clear

        let filter = SCContentFilter(desktopIndependentWindow: window)
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter,
                                                               configuration: configuration)
        return CapturedImage(image: image, scale: scale)
    }
}
