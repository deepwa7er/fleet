import CoreGraphics

/// Screen Recording is the TCC permission ScreenCaptureKit gates on. Nothing in
/// Shutter works without it, and unlike Accessibility it cannot be polled into
/// usefulness: the grant only takes effect for a *newly launched* process, so
/// the app has to notice the change and offer a relaunch rather than quietly
/// carrying on with a handle it will never be allowed to use.
enum Permissions {
    static var hasScreenRecording: Bool { CGPreflightScreenCaptureAccess() }

    /// Triggers the system prompt the first time; afterwards it just reports
    /// status, since only System Settings can flip a denied grant.
    @discardableResult
    static func requestScreenRecording() -> Bool { CGRequestScreenCaptureAccess() }
}
