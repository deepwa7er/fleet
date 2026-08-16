import AppKit

// LiveManager — owns the livestream lifecycle: hotkey → region picker → capture → ingest → control bar.
// Integrated from AppDelegate via `LiveManager.shared`.

@MainActor
final class LiveManager {
    static let shared = LiveManager()
    private var picker: LiveRegionPicker?
    private var session: LiveCaptureSession?
    private var ingest: LiveIngestClient?
    private var controlBar: LiveControlBar?
    private var hotKeyRef: EventHotKeyRef?
    private var handlerRef: EventHandlerRef?

    var isLive: Bool { session?.isRunning == true }

    func registerHotKey() {
        // Carbon hotkey for ⇧⌘9 — does not need Accessibility.
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        let handler: EventHandlerUPP = { _, event, userData in
            guard let event, let userData else { return noErr }
            var id = EventHotKeyID()
            GetEventParameter(event, EventParamName(kEventParamDirectObject), EventParamType(typeEventHotKeyID), nil, MemoryLayout<EventHotKeyID>.size, nil, &id)
            guard id.signature == 0x53485554, id.id == LiveShortcut.hotKeyID else { return noErr }
            // Dispatch to main actor
            DispatchQueue.main.async { LiveManager.shared.startOrStop() }
            return noErr
        }
        InstallEventHandler(GetApplicationEventTarget(), handler, 1, &spec, nil, &handlerRef)
        var ref: EventHotKeyRef?
        let status = RegisterEventHotKey(LiveShortcut.keyCode, LiveShortcut.modifiers, LiveShortcut.eventID, GetApplicationEventTarget(), 0, &ref)
        if status == noErr, let ref {
            hotKeyRef = ref
            Log.info("live hotkey \(LiveShortcut.label) registered")
        } else {
            Log.error("could not register live hotkey \(LiveShortcut.label) (status \(status))")
        }
    }

    func startOrStop() {
        if isLive { stop(); return }
        if picker != nil { return }
        startPicker()
    }

    private func startPicker() {
        let picker = LiveRegionPicker { [weak self] rect in
            self?.picker = nil
            guard let rect = rect else { return }
            Task { @MainActor in self?.startLive(rect: rect) }
        }
        self.picker = picker
        picker.show()
    }

    private func startLive(rect: CGRect) {
        let id = String(UUID().uuidString.prefix(8)).lowercased()
        let base = UserDefaults.standard.string(forKey: "ShutterRelayBase") ?? "wss://live.deepwa7er.com"
        let wsBase = base.replacingOccurrences(of: "https://", with: "wss://").replacingOccurrences(of: "http://", with: "ws://")
        var comps = URLComponents(string: wsBase + "/ingest")!
        comps.queryItems = [URLQueryItem(name: "id", value: id)]
        if let token = LiveSettings.ingestToken, !token.isEmpty {
            comps.queryItems?.append(URLQueryItem(name: "token", value: token))
        }
        guard let url = comps.url else { return }
        let ingest = LiveIngestClient(streamID: id, wsURL: url)
        ingest.token = LiveSettings.ingestToken
        self.ingest = ingest

        let session = LiveCaptureSession(rect: rect, onInitSegment: { [weak ingest] data in ingest?.sendInit(data) },
                                         onFragment: { [weak ingest] data in ingest?.sendFragment(data) })
        self.session = session

        Task {
            do {
                try await ingest.connect()
                try await session.start()
                let bar = LiveControlBar(streamID: id, rect: rect) { [weak self] in self?.stop() }
                self.controlBar = bar
                bar.show()
                let link = "https://live.deepwa7er.com/watch/\(id)"
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(link, forType: .string)
                Log.info("live at \(link)")
            } catch {
                Log.error("live failed: \(error)")
                await self.stop()
            }
        }
    }

    func stop() {
        controlBar?.dismiss(); controlBar = nil
        Task { await session?.stop() }
        session = nil
        Task { await ingest?.disconnect() }
        ingest = nil
    }
}

enum LiveSettings {
    static var ingestToken: String? {
        get {
            // Keychain stub — fallback to UserDefaults for dev
            UserDefaults.standard.string(forKey: "ShutterLiveToken")
        }
        set {
            if let v = newValue, !v.isEmpty {
                UserDefaults.standard.set(v, forKey: "ShutterLiveToken")
            } else {
                UserDefaults.standard.removeObject(forKey: "ShutterLiveToken")
            }
        }
    }
}
