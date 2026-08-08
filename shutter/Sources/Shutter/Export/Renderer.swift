import AppKit

enum Renderer {
    /// Burn the annotations into the capture at full resolution.
    ///
    /// Nothing is drawn when there is nothing to draw: an unannotated capture
    /// is returned exactly as it came off the screen, rather than being decoded
    /// and re-encoded through a context for no reason.
    static func compose(_ capture: CapturedImage, annotations: [Annotation]) throws -> CGImage {
        guard !annotations.isEmpty else { return capture.image }

        let width = capture.image.width
        let height = capture.image.height
        // Preserve the display's colour space where it is an RGB one, so a P3
        // capture is not silently clipped to sRGB on the way out.
        let space = capture.image.colorSpace.flatMap { $0.model == .rgb ? $0 : nil }
            ?? CGColorSpace(name: CGColorSpace.sRGB)!

        guard let ctx = CGContext(
            data: nil, width: width, height: height,
            bitsPerComponent: 8, bytesPerRow: 0, space: space,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { throw ShutterError.couldNotBuildBitmap }

        ctx.draw(capture.image, in: CGRect(x: 0, y: 0, width: width, height: height))

        let scale = capture.scale
        ctx.setStrokeColor(Annotation.strokeColor.cgColor)
        ctx.setLineWidth(Annotation.strokeWidth * scale)
        ctx.setLineJoin(.miter)
        for annotation in annotations {
            ctx.stroke(CGRect(x: annotation.rect.minX * scale, y: annotation.rect.minY * scale,
                              width: annotation.rect.width * scale,
                              height: annotation.rect.height * scale))
        }

        guard let image = ctx.makeImage() else { throw ShutterError.couldNotBuildBitmap }
        return image
    }
}
