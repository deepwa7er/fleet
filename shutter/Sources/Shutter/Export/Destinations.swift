import AppKit

/// Where a finished capture goes: the clipboard, and a PNG on disk.
enum Destinations {
    private static let saveDirectoryKey = "SaveDirectory"
    /// How long a copy-only backing file is kept before it is pruned. The
    /// clipboard is short-lived, so anything older than this is dead weight.
    private static let copyFileLifetime: TimeInterval = 24 * 60 * 60

    /// Defaults to the Desktop, matching where the system screenshot tool puts
    /// things. Stored as a path rather than a security-scoped bookmark because
    /// Shutter is not sandboxed and a bookmark would buy nothing.
    static var saveDirectory: URL {
        get {
            if let path = UserDefaults.standard.string(forKey: saveDirectoryKey) {
                return URL(fileURLWithPath: path, isDirectory: true)
            }
            return FileManager.default.urls(for: .desktopDirectory, in: .userDomainMask).first
                ?? FileManager.default.homeDirectoryForCurrentUser
        }
        set {
            UserDefaults.standard.set(newValue.path, forKey: saveDirectoryKey)
        }
    }

    /// Where a copy-only capture's backing file lives. Terminals and text
    /// fields can only paste a path, so the clipboard has to point at a real
    /// file; Caches is the right home for something that only matters while the
    /// user might still paste it.
    private static var copyDirectory: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first
            .map { $0.appendingPathComponent("Shutter", isDirectory: true) }
            ?? FileManager.default.temporaryDirectory
    }

    /// Put the capture on the clipboard, file-backed.
    ///
    /// The clipboard carries the path of a real PNG — as a file URL and as
    /// plain text, the two forms Finder writes when you copy a file — alongside
    /// the raw image data. Terminals and text fields paste the path; image-aware
    /// apps and terminals that understand image data paste the image itself.
    static func copyToClipboard(_ image: CGImage, pointSize: CGSize, file: URL) throws {
        let item = NSPasteboardItem()
        item.setString(file.absoluteString, forType: .fileURL)
        item.setString(file.path, forType: .string)
        item.setData(try pngData(image, pointSize: pointSize), forType: .png)
        // TIFF as well as PNG: a few older apps only offer to paste the former.
        if let tiff = bitmap(image, pointSize: pointSize).tiffRepresentation {
            item.setData(tiff, forType: .tiff)
        }

        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        guard pasteboard.writeObjects([item]) else { throw ShutterError.couldNotWriteClipboard }
    }

    @discardableResult
    static func save(_ image: CGImage, pointSize: CGSize) throws -> URL {
        try writePNG(image, pointSize: pointSize, to: saveDirectory)
    }

    /// Write a PNG for a copy-only capture and return its URL.
    static func copyFile(_ image: CGImage, pointSize: CGSize) throws -> URL {
        pruneCopyDirectory()
        return try writePNG(image, pointSize: pointSize, to: copyDirectory)
    }

    // MARK: Encoding

    private static func bitmap(_ image: CGImage, pointSize: CGSize) -> NSBitmapImageRep {
        let rep = NSBitmapImageRep(cgImage: image)
        // The rep comes back sized in pixels. Restating it in points is what
        // records 144 DPI in the PNG for a Retina capture, so it opens at its
        // true physical size instead of twice it.
        rep.size = pointSize
        return rep
    }

    private static func pngData(_ image: CGImage, pointSize: CGSize) throws -> Data {
        guard let data = bitmap(image, pointSize: pointSize).representation(using: .png,
                                                                            properties: [:])
        else { throw ShutterError.couldNotEncodePNG }
        return data
    }

    private static func writePNG(_ image: CGImage, pointSize: CGSize, to directory: URL) throws -> URL {
        let data = try pngData(image, pointSize: pointSize)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = uniqueURL(in: directory)
        try data.write(to: url, options: .atomic)
        return url
    }

    // MARK: Naming

    private static let formatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd 'at' HH.mm.ss"
        return formatter
    }()

    private static func uniqueURL(in directory: URL) -> URL {
        let stamp = formatter.string(from: Date())
        let base = "Shutter \(stamp)"
        var url = directory.appendingPathComponent("\(base).png")
        var suffix = 2
        // Two captures inside the same second would otherwise overwrite.
        while FileManager.default.fileExists(atPath: url.path) {
            url = directory.appendingPathComponent("\(base) (\(suffix)).png")
            suffix += 1
        }
        return url
    }

    // MARK: Housekeeping

    /// Remove copy-only backing files nobody can be pasting anymore. Without
    /// this the copy directory would grow without bound. Best effort: a file
    /// that cannot be read or removed is simply left in place.
    private static func pruneCopyDirectory() {
        let cutoff = Date().addingTimeInterval(-copyFileLifetime)
        let manager = FileManager.default
        guard let names = try? manager.contentsOfDirectory(atPath: copyDirectory.path) else { return }
        for name in names {
            let url = copyDirectory.appendingPathComponent(name)
            guard let modified = try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate,
                  modified < cutoff else { continue }
            try? manager.removeItem(at: url)
        }
    }
}
