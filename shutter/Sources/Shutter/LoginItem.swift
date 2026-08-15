import ServiceManagement

/// Registration of this bundle as a login item, so the capture hot key is live
/// on a freshly booted machine without a visit to the Applications folder.
///
/// `SMAppService.mainApp` writes to the same list System Settings → General →
/// Login Items shows, which means the user can revoke it from there behind our
/// back. `status` is therefore the only source of truth — never cache the flag
/// in preferences, or the menu will disagree with the system.
///
/// Registration records the bundle's current path. Moving or deleting the app
/// invalidates it (`status` drops to `.notFound`), so enable this on the copy
/// you keep for good, not on a build directory.
enum LoginItem {
    static var status: SMAppService.Status { SMAppService.mainApp.status }

    /// True once the login item exists *and* the user hasn't switched it off.
    static var isEnabled: Bool { status == .enabled }

    static func setEnabled(_ enabled: Bool) throws {
        if enabled {
            try SMAppService.mainApp.register()
        } else {
            try SMAppService.mainApp.unregister()
        }
    }

    /// Reveals the list in System Settings. Needed for `.requiresApproval`,
    /// which only the user can clear.
    static func openSystemSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }
}
