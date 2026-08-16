import AppKit

// LiveControlBar — floating pill while live: ● LIVE  W×H  30fps  [Copy link] [Stop]

final class LiveControlBar: NSObject {
    private var panel: NSPanel!
    private var onStop: () -> Void

    init(streamID: String, rect: CGRect, onStop: @escaping () -> Void) {
        self.onStop = onStop
        super.init()
        let w: CGFloat = 420, h: CGFloat = 36
        let screenFrame = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        let origin = NSPoint(x: screenFrame.midX - w/2, y: screenFrame.maxY - h - 20)
        panel = NSPanel(contentRect: NSRect(x: origin.x, y: origin.y, width: w, height: h),
                        styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.level = .floating
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = true
        panel.isReleasedWhenClosed = false

        let bg = NSView(frame: NSRect(x: 0, y: 0, width: w, height: h))
        bg.wantsLayer = true
        bg.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.85).cgColor
        bg.layer?.cornerRadius = 8
        bg.layer?.borderWidth = 1
        bg.layer?.borderColor = NSColor.white.withAlphaComponent(0.25).cgColor

        let label = NSTextField(labelWithString: "● LIVE  \(Int(rect.width))×\(Int(rect.height))  30fps")
        label.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .medium)
        label.textColor = .white
        label.frame = NSRect(x: 12, y: 10, width: 220, height: 16)
        bg.addSubview(label)

        let copyBtn = NSButton(title: "Copy link", target: self, action: #selector(copyLink))
        copyBtn.bezelStyle = .rounded
        copyBtn.font = NSFont.systemFont(ofSize: 11)
        copyBtn.frame = NSRect(x: 240, y: 6, width: 80, height: 24)
        bg.addSubview(copyBtn)

        let stopBtn = NSButton(title: "Stop", target: self, action: #selector(stopTapped))
        stopBtn.bezelStyle = .rounded
        stopBtn.font = NSFont.systemFont(ofSize: 11, weight: .semibold)
        stopBtn.contentTintColor = NSColor.systemRed
        stopBtn.frame = NSRect(x: 330, y: 6, width: 70, height: 24)
        bg.addSubview(stopBtn)

        panel.contentView = bg
        panel.title = "https://live.deepwa7er.com/watch/\(streamID)"
    }

    func show() { panel.orderFront(nil) }
    func dismiss() { panel.orderOut(nil) }

    @objc private func copyLink() {
        let link = panel.title
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(link, forType: .string)
    }

    @objc private func stopTapped() { onStop() }
}
