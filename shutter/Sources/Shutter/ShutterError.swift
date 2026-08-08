import Foundation

enum ShutterError: LocalizedError {
    case noDisplays
    case regionOutsideDisplay
    case couldNotEncodePNG
    case couldNotBuildBitmap
    case couldNotWriteClipboard

    var errorDescription: String? {
        switch self {
        case .noDisplays:
            return "No displays are available to capture."
        case .regionOutsideDisplay:
            return "The selected region does not overlap the display it was drawn on."
        case .couldNotEncodePNG:
            return "The capture could not be encoded as PNG."
        case .couldNotBuildBitmap:
            return "The capture could not be rendered."
        case .couldNotWriteClipboard:
            return "The capture could not be written to the clipboard."
        }
    }
}
