import CoreGraphics
import Foundation

// Probe: are any windows sitting exactly on the solo tile (screen inset by
// the gap)? If yes, the standalone Carousel instance has AX permission and
// has enrolled windows. If no, it's running ungranted and doing nothing.
// Keep in sync with CarouselLayout.gap.
let gap: CGFloat = 10
guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] else {
    print("no window list"); exit(1)
}
var tiled = 0, offscreen = 0, total = 0
for info in list {
    guard let layer = info["kCGWindowLayer"] as? Int, layer == 0,
          let owner = info["kCGWindowOwnerName"] as? String,
          !["Window Server", "Dock", "Control Center", "WindowManager", "Carousel"].contains(owner),
          let b = info["kCGWindowBounds"] as? [String: Any],
          let r = CGRect(dictionaryRepresentation: b as CFDictionary) else { continue }
    total += 1
    let name = info["kCGWindowTitle"] as? String ?? ""
    if abs(r.minX - gap) < 2 && abs(r.minY - gap - 25) < 30 {  // ~menu-bar adjusted
        tiled += 1
        print("TILED:  \(owner) — \(name) \(r)")
    } else if r.minX > 1500 || r.maxX < 0 {
        offscreen += 1
        print("PARKED: \(owner) — \(name) \(r)")
    } else {
        print("free:   \(owner) — \(name) \(r)")
    }
}
print("== \(tiled) tiled, \(offscreen) parked off-screen, \(total) layer-0 windows")
