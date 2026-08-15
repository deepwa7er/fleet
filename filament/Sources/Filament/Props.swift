/// The attributes attached to a host element.
///
/// Props are deliberately a closed set of values rather than `[String: Any]`.
/// The reconciler must be able to answer "did this prop change?" without the
/// host's help, and `Any` cannot be compared.
public struct Props: ExpressibleByDictionaryLiteral {
    public enum Value {
        case string(String)
        case number(Double)
        case bool(Bool)
        /// An event handler. Handlers are opaque and are re-bound on every
        /// render — see `changed(from:)`.
        case handler(@MainActor () -> Void)
    }

    public private(set) var storage: [String: Value]

    public init(dictionaryLiteral elements: (String, Value)...) {
        storage = Dictionary(elements, uniquingKeysWith: { _, last in last })
    }

    public init(_ storage: [String: Value] = [:]) {
        self.storage = storage
    }

    public subscript(key: String) -> Value? { storage[key] }

    /// The props that must be applied to move a host instance from `old` to
    /// `self`, plus the props that must be cleared.
    ///
    /// Handlers always appear in `updated`: two closures are never comparable in
    /// Swift, so the only sound answer is to assume every handler is new. React
    /// reaches the same conclusion for the same reason, which is why an inline
    /// arrow function defeats prop-equality bailouts there too.
    public func diff(from old: Props) -> (updated: [String: Value], removed: [String]) {
        var updated: [String: Value] = [:]
        for (key, value) in storage {
            guard let previous = old.storage[key] else {
                updated[key] = value
                continue
            }
            if !value.matches(previous) { updated[key] = value }
        }
        let removed = old.storage.keys.filter { storage[$0] == nil }
        return (updated, Array(removed))
    }
}

extension Props.Value {
    /// Structural comparison. Returns `false` for any pair involving a handler.
    func matches(_ other: Props.Value) -> Bool {
        switch (self, other) {
        case let (.string(a), .string(b)): a == b
        case let (.number(a), .number(b)): a == b
        case let (.bool(a), .bool(b)): a == b
        case (.handler, .handler): false
        default: false
        }
    }

    /// A printable form, used by the test host for snapshotting.
    public var description: String {
        switch self {
        case .string(let s): "\"\(s)\""
        case .number(let n): n == n.rounded() ? String(Int(n)) : String(n)
        case .bool(let b): String(b)
        case .handler: "<handler>"
        }
    }
}
