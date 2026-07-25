import CoreGraphics

/// Login-session state the ring has to respect.
enum Session {
    /// Whether the screen is locked.
    ///
    /// While the lock screen is up the WindowServer stops reporting the user's
    /// windows as on-screen, so anything derived from `CGWindowListCopyWindowInfo`
    /// describes the lock screen rather than the session behind it.
    ///
    /// `CGSessionCopyCurrentDictionary` is public API; the key carrying lock
    /// state is not headered, but it is the long-standing way to read it. A
    /// missing key reads as unlocked, which is the safe default — that is just
    /// the behaviour of an ungated ring.
    static var screenIsLocked: Bool {
        guard let session = CGSessionCopyCurrentDictionary() as? [String: Any] else { return false }
        return session["CGSSessionScreenIsLocked"] as? Bool ?? false
    }
}
