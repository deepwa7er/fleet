import AppKit

/// A finished capture on its way to the editor.
///
/// The pixel buffer alone is not enough to work with: a 2560×1440 image is a
/// 1280×720 window on a Retina display and a 2560×1440 one on an external
/// monitor, and the editor has to lay out annotations in the logical space the
/// user actually pointed at. `scale` carries that ratio.
struct CapturedImage {
    let image: CGImage
    /// Pixels per point in `image`.
    let scale: CGFloat

    var pixelSize: CGSize { CGSize(width: CGFloat(image.width), height: CGFloat(image.height)) }
    var pointSize: CGSize { CGSize(width: pixelSize.width / scale, height: pixelSize.height / scale) }
}
