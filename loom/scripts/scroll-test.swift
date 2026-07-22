import CoreGraphics
import Foundation

// Debug harness: post one ⌥+scroll step and sample frames for 1.5s, reporting
// whether any window MOVED at any point (net displacement can be zero by
// symmetry, e.g. two windows swapping places).
func frames() -> [UInt32: CGRect] {
    var out: [UInt32: CGRect] = [:]
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]]
    else { return out }
    for info in list {
        guard let id = info["kCGWindowNumber"] as? UInt32,
              let b = info["kCGWindowBounds"] as? [String: Any],
              let rect = CGRect(dictionaryRepresentation: b as CFDictionary) else { continue }
        out[id] = rect
    }
    return out
}

let before = frames()
let src = CGEventSource(stateID: .combinedSessionState)
if let ev = CGEvent(scrollWheelEvent2Source: src, units: .line, wheelCount: 1, wheel1: 1, wheel2: 0, wheel3: 0) {
    ev.flags = .maskAlternate
    ev.post(tap: .cghidEventTap)
}
var movedIDs = Set<UInt32>()
for _ in 0..<15 {
    Thread.sleep(forTimeInterval: 0.1)
    for (id, rect) in frames() where before[id] != nil && before[id] != rect {
        movedIDs.insert(id)
    }
}
print(movedIDs.isEmpty ? "no motion" : "MOTION: \(movedIDs.count) windows moved during animation")
