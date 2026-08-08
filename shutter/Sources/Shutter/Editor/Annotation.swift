import AppKit

/// A red rectangle drawn over a capture.
///
/// Stored in the capture's *point* space, bottom-left origin — the same space
/// the editor lays out in, and independent of how large the editor window
/// happens to be. Conversion to pixels happens once, at export.
struct Annotation: Equatable {
    var rect: CGRect

    static let strokeColor = NSColor(srgbRed: 1.0, green: 0.16, blue: 0.13, alpha: 1.0)
    /// Line width in capture points, so a mark looks the same weight whether it
    /// was drawn on a Retina display or an external monitor.
    static let strokeWidth: CGFloat = 3
}
