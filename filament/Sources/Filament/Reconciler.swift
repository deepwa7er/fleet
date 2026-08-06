/// Turns a stream of element trees into the minimum set of host mutations.
///
/// The pass is split in two, for the same reason React splits it:
///
/// * **Render phase** walks elements against the existing fiber tree, decides
///   what is new, updated, moved or gone, and flags fibers accordingly. It may
///   create host instances, but it never inserts them, because an insertion
///   needs to know where every sibling ends up and that is not known until the
///   whole child list has been reconciled.
/// * **Commit phase** walks the finished tree once, in order, applying
///   deletions and then placements.
///
/// Effects run last, after the host tree is consistent.
@MainActor
public final class Reconciler<Host: HostConfig> {
    private typealias F = Fiber<Host.Instance>

    private let host: Host
    private let container: Host.Instance

    private var root: F?
    private let effectQueue = EffectQueue()

    /// Fibers pending re-render, deduplicated by identity.
    private var dirty: [ObjectIdentifier: F] = [:]

    /// Subtrees removed during the render phase, paired with the host instance
    /// they must be detached from (captured before the tree link is severed).
    private var deletions: [(fiber: F, hostParent: Host.Instance?)] = []

    private var isFlushing = false

    /// Safety valve for a component that unconditionally sets state during
    /// render or in an effect, which would otherwise spin forever.
    private let maxFlushRounds = 50

    public init(host: Host, container: Host.Instance) {
        self.host = host
        self.container = container
    }

    // MARK: - Public API

    /// Renders `element` into the container, updating in place if this is not
    /// the first call.
    public func render(_ element: Element) {
        flushSynchronously {
            let root = ensureRoot()
            reconcileChildren(of: root, to: [element])
        }
    }

    /// Groups several state updates into a single render pass.
    public func batch(_ work: () -> Void) {
        flushSynchronously(work)
    }

    /// Tears down the whole tree, running every effect cleanup.
    public func unmountAll() {
        guard let root else { return }
        for child in root.children {
            deletions.append((child, container))
        }
        root.children = []
        commit()
        self.root = nil
    }

    // MARK: - Scheduling

    private func ensureRoot() -> F {
        if let root { return root }
        let created = F(kind: .root, typeIdentity: nil, key: nil, parent: nil)
        created.instance = container
        root = created
        return created
    }

    private func scheduleUpdate(_ fiber: F) {
        dirty[ObjectIdentifier(fiber)] = fiber
        guard !isFlushing else { return }
        flushSynchronously {}
    }

    /// Runs `work`, then keeps rendering until no further updates are pending.
    ///
    /// Reentrant calls (a `setState` inside an effect, say) simply run inline:
    /// their updates land in `dirty` and are picked up by the loop already in
    /// progress, which is what makes updates batch rather than cascade.
    private func flushSynchronously(_ work: () -> Void) {
        if isFlushing {
            work()
            return
        }

        isFlushing = true
        defer { isFlushing = false }

        work()
        commit()
        runEffects()

        var round = 0
        while !dirty.isEmpty {
            round += 1
            guard round <= maxFlushRounds else {
                fatalError(
                    """
                    Update did not settle after \(maxFlushRounds) rounds. A component \
                    is setting state on every render, or an effect is re-triggering \
                    itself — check that your useEffect dependencies are stable.
                    """
                )
            }

            // Parents first: a parent re-render may unmount a child that also
            // has a pending update, making the child's work unnecessary.
            let batch = dirty.values.sorted { $0.depth < $1.depth }
            dirty.removeAll()

            for fiber in batch where fiber.isMounted {
                rerender(fiber)
            }

            commit()
            runEffects()
        }
    }

    private func rerender(_ fiber: F) {
        guard case .component(let componentElement)? = fiber.element else { return }
        let output = render(component: componentElement, on: fiber)
        reconcileChildren(of: fiber, to: [output])
    }

    private func runEffects() {
        for effect in effectQueue.drain() {
            effect()
        }
    }

    // MARK: - Render phase

    private func render(component element: ComponentElement, on fiber: F) -> Element {
        HookContext.begin(fiber, effects: effectQueue)
        defer { HookContext.end() }
        return element.component.render()
    }

    /// Matches `elements` against `parent`'s current children.
    ///
    /// Matching is by key when the element has one and by position when it does
    /// not, which is exactly why a list rendered without keys loses state when
    /// reordered: position is the only identity such a child has.
    private func reconcileChildren(of parent: F, to elements: [Element]) {
        assertKeysAreUnique(elements)
        let old = parent.children

        // Each candidate carries the index it held, so the move check below
        // never has to look an index back up and never has an "index missing"
        // case to invent an answer for.
        var oldByKey: [AnyHashable: (fiber: F, index: Int)] = [:]
        var oldByPosition: [Int: (fiber: F, index: Int)] = [:]

        for (index, child) in old.enumerated() {
            if let key = child.key {
                oldByKey[key] = (child, index)
            } else {
                oldByPosition[index] = (child, index)
            }
        }

        var reused: Set<ObjectIdentifier> = []
        var newChildren: [F] = []

        // The high-water mark of reused old indices. An element whose old index
        // is behind it moved backwards relative to its siblings and therefore
        // needs repositioning; anything at or ahead of it stayed in order and
        // does not. This is what keeps a single insertion at the head of a list
        // from being reported as N moves.
        var lastPlacedIndex = 0

        for (newIndex, element) in elements.enumerated() {
            let candidate = element.key.flatMap { oldByKey[$0] } ?? oldByPosition[newIndex]

            if let (existing, oldIndex) = candidate,
               !reused.contains(ObjectIdentifier(existing)),
               existing.accepts(element) {
                reused.insert(ObjectIdentifier(existing))
                update(existing, with: element)

                if oldIndex < lastPlacedIndex {
                    existing.needsPlacement = true
                } else {
                    lastPlacedIndex = oldIndex
                }
                newChildren.append(existing)
            } else {
                let created = mount(element, parent: parent)
                created.needsPlacement = true
                newChildren.append(created)
            }
        }

        let hostParent = nearestHostInstance(from: parent)
        for child in old where !reused.contains(ObjectIdentifier(child)) {
            deletions.append((child, hostParent))
        }

        parent.children = newChildren
    }

    /// A key is an identity claim, and two siblings cannot both be the same
    /// child. Left undetected the ambiguity is resolved arbitrarily — one of
    /// the two gets the surviving fiber and the other is rebuilt from scratch,
    /// silently discarding its state.
    ///
    /// React warns here and proceeds; this traps. The check is deliberately not
    /// gated behind a debug flag, because a renderer that reconciles
    /// differently in debug and release is a worse problem than the cost of an
    /// O(n) scan over a list already being walked O(n) times.
    private func assertKeysAreUnique(_ elements: [Element]) {
        var seen: Set<AnyHashable> = []
        for element in elements {
            guard let key = element.key else { continue }
            guard seen.insert(key).inserted else {
                preconditionFailure(
                    """
                    Two sibling elements share the key \(key). A key is an \
                    identity, so duplicates leave no way to decide which child \
                    is which, and state would attach to an arbitrary one. Give \
                    each sibling in a list a distinct key.
                    """
                )
            }
        }
    }

    private func mount(_ element: Element, parent: F) -> F {
        switch element {
        case .text(let text):
            let fiber = F(kind: .text, typeIdentity: .text, key: nil, parent: parent)
            fiber.element = element
            fiber.instance = host.createText(text)
            connect(fiber)
            return fiber

        case .host(let hostElement):
            let fiber = F(
                kind: .host,
                typeIdentity: .host(hostElement.tag),
                key: hostElement.key,
                parent: parent
            )
            fiber.element = element
            fiber.instance = host.createInstance(tag: hostElement.tag, props: hostElement.props)
            connect(fiber)
            reconcileChildren(of: fiber, to: hostElement.children)
            return fiber

        case .component(let componentElement):
            let fiber = F(
                kind: .component,
                typeIdentity: .component(componentElement.typeID),
                key: componentElement.key,
                parent: parent
            )
            fiber.element = element
            connect(fiber)
            let output = render(component: componentElement, on: fiber)
            reconcileChildren(of: fiber, to: [output])
            return fiber
        }
    }

    private func connect(_ fiber: F) {
        fiber.requestUpdate = { [weak self, weak fiber] in
            guard let self, let fiber else { return }
            scheduleUpdate(fiber)
        }
    }

    /// Updates a fiber in place. The caller has already established that the
    /// fiber accepts this element, so type and key are known to match.
    private func update(_ fiber: F, with element: Element) {
        let previous = fiber.element
        fiber.element = element

        switch (element, previous) {
        case (.text(let text), .text(let oldText)?):
            if text != oldText, let instance = fiber.instance {
                host.updateText(instance, text: text)
            }

        case (.host(let new), .host(let old)?):
            if let instance = fiber.instance {
                let (updated, removed) = new.props.diff(from: old.props)
                if !updated.isEmpty || !removed.isEmpty {
                    host.updateInstance(instance, updated: updated, removed: removed)
                }
            }
            reconcileChildren(of: fiber, to: new.children)

        case (.component(let new), .component?):
            // No props-equality bailout here, matching React's default: a
            // re-rendered parent re-renders its children unless explicitly
            // memoized. Adding `memo` would be a bailout check right here.
            let output = render(component: new, on: fiber)
            reconcileChildren(of: fiber, to: [output])

        default:
            preconditionFailure(
                "update(_:with:) reached a mismatched element pair, which means "
                + "`accepts` and this switch have drifted apart."
            )
        }
    }

    // MARK: - Commit phase

    private func commit() {
        for (fiber, hostParent) in deletions {
            detach(fiber, from: hostParent)
        }
        deletions.removeAll()

        if let root {
            commitPlacements(root)
        }
    }

    private func detach(_ fiber: F, from hostParent: Host.Instance?) {
        // Cleanups run innermost-first, so a child never observes a parent that
        // has already torn itself down.
        unmountEffects(fiber)

        if let hostParent {
            for top in topHostFibers(of: fiber) {
                if let instance = top.instance {
                    host.remove(instance, from: hostParent)
                }
            }
        }

        markUnmounted(fiber)
    }

    private func unmountEffects(_ fiber: F) {
        for child in fiber.children {
            unmountEffects(child)
        }
        for slot in fiber.hooks {
            (slot as? EffectSlot)?.cleanup?()
        }
    }

    private func markUnmounted(_ fiber: F) {
        fiber.isMounted = false
        for child in fiber.children {
            markUnmounted(child)
        }
        fiber.children = []
        fiber.hooks = []
    }

    /// Pre-order walk applying placements. Order matters: a fiber is placed
    /// only after everything to its left is already positioned, so the anchor
    /// search below can trust anything it finds.
    private func commitPlacements(_ fiber: F) {
        if fiber.needsPlacement {
            let hostParent = fiber.parent.flatMap { nearestHostInstance(from: $0) }
            let anchor = hostAnchor(after: fiber)

            for top in topHostFibers(of: fiber) {
                if let instance = top.instance, let hostParent {
                    host.insert(instance, into: hostParent, before: anchor)
                }
                top.needsPlacement = false
            }
            fiber.needsPlacement = false
        }

        for child in fiber.children {
            commitPlacements(child)
        }
    }

    /// The next host instance after `fiber` in tree order that is already in
    /// its final position.
    ///
    /// Fibers still awaiting placement are skipped deliberately — inserting
    /// before a node that is itself about to move would pin the new node to the
    /// wrong spot.
    private func hostAnchor(after fiber: F) -> Host.Instance? {
        var node = fiber
        while let parent = node.parent {
            if let index = parent.children.firstIndex(where: { $0 === node }) {
                for sibling in parent.children[(index + 1)...] {
                    if let found = firstPlacedHostInstance(in: sibling) {
                        return found
                    }
                }
            }
            // A host boundary ends the search: there is no sibling beyond the
            // end of the parent element's own child list.
            if parent.kind == .host || parent.kind == .root { return nil }
            node = parent
        }
        return nil
    }

    private func firstPlacedHostInstance(in fiber: F) -> Host.Instance? {
        if fiber.needsPlacement { return nil }
        if fiber.kind == .host || fiber.kind == .text { return fiber.instance }
        for child in fiber.children {
            if let found = firstPlacedHostInstance(in: child) { return found }
        }
        return nil
    }

    /// The shallowest host-backed fibers in a subtree.
    ///
    /// A component renders no instance of its own, so "move this component"
    /// means moving each of the host nodes at the top of what it produced.
    private func topHostFibers(of fiber: F) -> [F] {
        if fiber.kind == .host || fiber.kind == .text { return [fiber] }
        return fiber.children.flatMap { topHostFibers(of: $0) }
    }

    private func nearestHostInstance(from fiber: F) -> Host.Instance? {
        var node: F? = fiber
        while let current = node {
            switch current.kind {
            case .root: return container
            case .host: return current.instance
            case .text, .component: node = current.parent
            }
        }
        return nil
    }
}
