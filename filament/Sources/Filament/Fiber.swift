/// A node in the persistent tree that mirrors the element tree.
///
/// Elements are thrown away and rebuilt on every render; fibers survive. That
/// is the entire trick behind hooks: `useState` needs somewhere to put a value
/// that outlives the function call which created it, and the fiber is that
/// somewhere.
@MainActor
final class Fiber<Instance: AnyObject> {
    enum Kind {
        case root
        case text
        case host
        case component
        /// Contributes children to an ancestor's host instance without owning
        /// one of its own — like `.component`, but with no render function.
        case fragment
    }

    let kind: Kind
    let typeIdentity: TypeIdentity?
    let key: AnyHashable?

    /// The element that produced the current state of this fiber.
    var element: Element?

    /// The backing host node, for `.text` and `.host` fibers only. Component
    /// and root fibers render into their nearest host *ancestor*.
    var instance: Instance?

    /// Hook storage, indexed by call order within `render()`.
    var hooks: [any HookSlot] = []

    weak var parent: Fiber?
    var children: [Fiber] = []

    /// Depth from the root, used to flush pending updates parents-first so a
    /// parent re-render that unmounts a child makes the child's own pending
    /// update moot rather than doubly applied.
    let depth: Int

    /// Set to `false` when the fiber is unmounted, so a `setState` captured by
    /// an already-detached closure becomes a no-op instead of resurrecting it.
    var isMounted = true

    /// Flagged during the render phase when this fiber is newly created or has
    /// moved relative to its siblings, and cleared when the commit phase has
    /// inserted its host instances in the right position.
    var needsPlacement = false

    /// Requests a re-render of this fiber. Injected by the reconciler so the
    /// fiber never has to know the reconciler's generic parameters.
    var requestUpdate: (@MainActor () -> Void)?

    init(kind: Kind, typeIdentity: TypeIdentity?, key: AnyHashable?, parent: Fiber?) {
        self.kind = kind
        self.typeIdentity = typeIdentity
        self.key = key
        self.parent = parent
        self.depth = parent.map { $0.depth + 1 } ?? 0
    }

    /// Whether `element` can update this fiber in place, or whether the fiber
    /// must be discarded and rebuilt.
    func accepts(_ element: Element) -> Bool {
        typeIdentity == element.typeIdentity && key == element.key
    }
}
