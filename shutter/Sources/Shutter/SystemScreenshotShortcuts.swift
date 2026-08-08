import Foundation

/// Switches macOS's own screenshot shortcuts off so Shutter can answer them.
///
/// ⌘⇧3/4/5 are *symbolic hot keys*: the window server claims them before any
/// application hot key is dispatched. `RegisterEventHotKey` for those
/// combinations is accepted and returns `noErr` — and then never fires. The
/// only way for Shutter to receive them is for the system to stop taking them.
///
/// `CGSSetSymbolicHotKeyEnabled` is private SkyLight API. It is resolved
/// through `dlsym` rather than linked against, so a future macOS that drops it
/// leaves Shutter running with a clear diagnostic and a fallback message
/// instead of failing to launch.
///
/// The change outlives the process, which makes losing track of it the real
/// hazard: an unclean exit would leave the system shortcuts switched off with
/// nothing left to switch them back on. So the prior state of every key is
/// recorded in preferences *before* anything is touched, and a record found at
/// launch is restored from before a fresh takeover — an unclean exit repairs
/// itself on the next run.
enum SystemScreenshotShortcuts {
    private static let storageKey = "DisplacedSymbolicHotKeys"

    private typealias IsEnabled = @convention(c) (Int32) -> Bool
    private typealias SetEnabled = @convention(c) (Int32, Bool) -> Int32

    private static let symbols: (isEnabled: IsEnabled, setEnabled: SetEnabled)? = {
        guard let handle = dlopen(
                "/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY),
              let isEnabled = dlsym(handle, "CGSIsSymbolicHotKeyEnabled"),
              let setEnabled = dlsym(handle, "CGSSetSymbolicHotKeyEnabled")
        else {
            Log.error("SkyLight's symbolic hot key API is unavailable; the system screenshot "
                      + "shortcuts cannot be displaced from inside Shutter")
            return nil
        }
        return (unsafeBitCast(isEnabled, to: IsEnabled.self),
                unsafeBitCast(setEnabled, to: SetEnabled.self))
    }()

    /// False when the private API could not be resolved, in which case the user
    /// has to turn the shortcuts off in System Settings by hand.
    static var isAvailable: Bool { symbols != nil }

    static var isTakenOver: Bool {
        UserDefaults.standard.dictionary(forKey: storageKey) != nil
    }

    static func takeOver(_ ids: [Int32]) {
        guard let symbols else { return }
        // Repair an unclean exit before recording anything new, or the prior
        // state we save would be the disabled state we ourselves left behind.
        restore()

        var previous: [String: Bool] = [:]
        for id in ids {
            previous[String(id)] = symbols.isEnabled(id)
        }
        // Record first, act second. A crash in between leaves a record
        // describing more than was changed, which `restore` handles harmlessly;
        // the other order could leave a disabled key that nothing remembers.
        UserDefaults.standard.set(previous, forKey: storageKey)

        for id in ids {
            let status = symbols.setEnabled(id, false)
            if status != 0 {
                Log.error("could not disable system shortcut \(id) (status \(status))")
            }
        }
        Log.info("took over system screenshot shortcuts \(ids.map(String.init).joined(separator: ", "))")
    }

    static func restore() {
        guard let symbols,
              let stored = UserDefaults.standard.dictionary(forKey: storageKey) as? [String: Bool]
        else { return }

        for (key, wasEnabled) in stored {
            guard let id = Int32(key) else { continue }
            // Restore to what was found, not to "on": a shortcut the user had
            // already switched off in System Settings stays off.
            _ = symbols.setEnabled(id, wasEnabled)
        }
        UserDefaults.standard.removeObject(forKey: storageKey)
        Log.info("restored the system screenshot shortcuts")
    }
}
