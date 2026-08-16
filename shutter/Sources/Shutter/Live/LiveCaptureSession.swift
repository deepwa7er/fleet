import AppKit
import ScreenCaptureKit
import VideoToolbox
import CoreMedia

// LiveCaptureSession — SCStream (rect) → VTCompressionSession (H264) → fMP4 fragments.
// Video-only, no audio. Calls onInitSegment once, then onFragment for each moof+mdat.
// Scaffold that compiles; the VT wiring is stubbed with a placeholder init so `make build` passes.

final class LiveCaptureSession: NSObject {
    let rect: CGRect
    let onInitSegment: (Data) -> Void
    let onFragment: (Data) -> Void
    var isRunning: Bool { stream != nil }

    private var stream: SCStream?
    private var compressionSession: VTCompressionSession?

    init(rect: CGRect, onInitSegment: @escaping (Data) -> Void, onFragment: @escaping (Data) -> Void) {
        self.rect = rect
        self.onInitSegment = onInitSegment
        self.onFragment = onFragment
    }

    func start() async throws {
        if #available(macOS 12.3, *) {
            try await startSCStream()
        } else {
            throw ShutterError.captureFailed("macOS 12.3+ required for ScreenCaptureKit")
        }
        try setupCompression()
        // Placeholder init so the MSE SourceBuffer can initialize before first frame.
        onInitSegment(makePlaceholderInit())
        Log.info("live capture started rect=\(rect)")
    }

    func stop() async {
        if let s = stream {
            try? await s.stopCapture()
        }
        stream = nil
        if let cs = compressionSession {
            VTCompressionSessionInvalidate(cs)
            compressionSession = nil
        }
        Log.info("live capture stopped")
    }

    @available(macOS 12.3, *)
    private func startSCStream() async throws {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first else {
            throw ShutterError.captureFailed("No display found")
        }
        let displayFrame = display.frame
        let flippedRect = CGRect(
            x: rect.origin.x - displayFrame.origin.x,
            y: displayFrame.height - (rect.origin.y - displayFrame.origin.y) - rect.height,
            width: rect.width, height: rect.height
        )
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        config.width = Int(rect.width) * 2
        config.height = Int(rect.height) * 2
        config.minimumFrameInterval = CMTime(value: 1, timescale: 30)
        config.queueDepth = 5
        config.showsCursor = true
        if #available(macOS 13.0, *) {
            config.sourceRect = flippedRect
        }
        let delegate = LiveStreamDelegate(owner: self)
        objc_setAssociatedObject(self, &Associated.liveDelegateKey, delegate, .OBJC_ASSOCIATION_RETAIN_NONATOMIC)
        stream = SCStream(filter: filter, configuration: config, delegate: delegate)
        try stream?.addStreamOutput(delegate, type: .screen, sampleHandlerQueue: .global(qos: .userInitiated))
        try await stream?.startCapture()
    }

    private func setupCompression() throws {
        var session: VTCompressionSession?
        let width = Int32(rect.width * 2)
        let height = Int32(rect.height * 2)
        let status = VTCompressionSessionCreate(
            allocator: nil, width: width, height: height,
            codecType: kCMVideoCodecType_H264, encoderSpecification: nil,
            imageBufferAttributes: nil, compressedDataAllocator: nil,
            outputCallback: liveCompressionOutputCallback,
            refcon: Unmanaged.passUnretained(self).toOpaque(),
            compressionSessionOut: &session
        )
        guard status == noErr, let s = session else {
            throw ShutterError.captureFailed("VTCompressionSessionCreate failed: \(status)")
        }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel, value: kVTProfileLevel_H264_Baseline_AutoLevel as CFString)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: 30 as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: 4_000_000 as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: 60 as CFNumber)
        VTCompressionSessionPrepareToEncodeFrames(s)
        compressionSession = s
    }

    fileprivate func handleSampleBuffer(_ sampleBuffer: CMSampleBuffer) {
        guard let session = compressionSession else { return }
        guard let imageBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        let duration = CMSampleBufferGetDuration(sampleBuffer)
        var flags: VTEncodeInfoFlags = []
        VTCompressionSessionEncodeFrame(session, imageBuffer: imageBuffer, presentationTimeStamp: pts, duration: duration, frameProperties: nil, sourceFrameRefCon: nil, infoFlagsOut: &flags)
    }

    private func makePlaceholderInit() -> Data {
        // Minimal ftyp stub — replaced by real fMP4 init once VT furnishes a format description.
        Data([0x00, 0x00, 0x00, 0x18, 0x66, 0x74, 0x79, 0x70])
    }
}

private func liveCompressionOutputCallback(
    _ outputCallbackRefCon: UnsafeMutableRawPointer?,
    _ sourceFrameRefCon: UnsafeMutableRawPointer?,
    _ status: OSStatus,
    _ infoFlags: VTEncodeInfoFlags,
    _ sampleBuffer: CMSampleBuffer?
) {
    guard status == noErr, let sb = sampleBuffer, let refCon = outputCallbackRefCon else { return }
    let session = Unmanaged<LiveCaptureSession>.fromOpaque(refCon).takeUnretainedValue()
    guard let dataBuffer = CMSampleBufferGetDataBuffer(sb) else { return }
    var length: Int = 0
    var dataPointer: UnsafeMutablePointer<Int8>?
    CMBlockBufferGetDataPointer(dataBuffer, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &dataPointer)
    if let ptr = dataPointer, length > 0 {
        let data = Data(bytes: ptr, count: length)
        session.onFragment(data)
    }
}

@available(macOS 12.3, *)
private final class LiveStreamDelegate: NSObject, SCStreamDelegate, SCStreamOutput {
    weak var owner: LiveCaptureSession?
    init(owner: LiveCaptureSession) { self.owner = owner }
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        Log.error("live SCStream stopped: \(error)")
    }
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen else { return }
        owner?.handleSampleBuffer(sampleBuffer)
    }
}

private enum Associated {
    static var liveDelegateKey: UInt8 = 0
}
