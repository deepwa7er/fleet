import Testing
@testable import Filament

// MARK: - Comparing host trees

/// A structural view of a host tree.
///
/// `TestNode` compares by identity, which is the right default everywhere else
/// — it is how the unit tests prove a node was updated rather than replaced.
/// Here the question is the opposite one: do two independently built trees have
/// the same shape?
struct Snapshot: Equatable {
    let tag: String
    let text: String?
    let props: [String: String]
    let children: [Snapshot]

    func describe(indent: Int = 0) -> String {
        let pad = String(repeating: "  ", count: indent)
        var line: String
        if let text {
            line = "\(pad)\"\(text)\""
        } else {
            let attributes = props.sorted { $0.key < $1.key }
                .map { "\($0.key)=\($0.value)" }
                .joined(separator: " ")
            line = attributes.isEmpty ? "\(pad)<\(tag)>" : "\(pad)<\(tag) \(attributes)>"
        }
        for child in children { line += "\n" + child.describe(indent: indent + 1) }
        return line
    }
}

extension TestNode {
    var snapshot: Snapshot {
        Snapshot(
            tag: tag,
            text: text,
            props: props.storage.mapValues(\.description),
            children: children.map(\.snapshot)
        )
    }
}

/// One reconciler and the host tree it owns.
@MainActor
final class World {
    let host: TestHost
    let container: TestNode
    let renderer: Reconciler<TestHost>
    let ledger = Ledger()

    init() {
        let host = TestHost()
        let container = host.makeContainer()
        self.host = host
        self.container = container
        self.renderer = Reconciler(host: host, container: container)
    }

    var log: [String] { host.log }
    func clearLog() { host.clearLog() }

    func render(_ shape: Shape) {
        renderer.render(shape.element(ledger: ledger))
    }

    var snapshot: Snapshot { container.snapshot }
}

@MainActor
private func freshSnapshot(of shape: Shape) -> Snapshot {
    let world = World()
    world.render(shape)
    return world.snapshot
}

/// Returns the first node reachable twice from `root`, if any.
private func firstDuplicate(under root: TestNode) -> TestNode? {
    var seen: Set<ObjectIdentifier> = []
    var stack = [root]
    while let node = stack.popLast() {
        if !seen.insert(ObjectIdentifier(node)).inserted { return node }
        stack.append(contentsOf: node.children)
    }
    return nil
}

// MARK: - Properties over arbitrary trees

@Suite("Properties")
@MainActor
struct PropertyTests {
    /// The central claim. Everything else the reconciler does is an
    /// optimisation on top of this: however it got there, the tree it arrives
    /// at incrementally must be the tree you would have built from scratch.
    @Test("an incrementally diffed tree always equals a fresh render")
    func convergence() {
        forAll("incremental and fresh renders agree") { scenario in
            let world = World()
            for (index, shape) in scenario.steps.enumerated() {
                world.render(shape)
                let expected = freshSnapshot(of: shape)
                guard world.snapshot != expected else { continue }
                return """
                    At step \(index) the incremental tree diverged from a fresh render.

                      incremental:
                    \(world.snapshot.describe(indent: 2))

                      fresh:
                    \(expected.describe(indent: 2))
                    """
            }
            return nil
        }
    }

    /// Rendering the same description twice must be free. This is the property
    /// that would catch a diff quietly rebuilding what it could have kept.
    @Test("re-rendering an unchanged tree performs no host work")
    func idempotence() {
        forAll("a repeated render is a no-op") { scenario in
            let world = World()
            for (index, shape) in scenario.steps.enumerated() {
                world.render(shape)
                world.clearLog()
                world.render(shape)
                guard !world.log.isEmpty else { continue }
                return """
                    Re-rendering step \(index) unchanged produced \(world.log.count) \
                    mutation(s):
                      \(world.log.joined(separator: "\n      "))
                    """
            }
            return nil
        }
    }

    /// A node reachable twice means an insert happened without the matching
    /// detach — the failure mode that a naive placement pass produces and that
    /// a snapshot comparison alone can miss.
    @Test("the host tree stays a tree")
    func structuralIntegrity() {
        forAll("no node is reachable twice") { scenario in
            let world = World()
            for (index, shape) in scenario.steps.enumerated() {
                world.render(shape)
                if let duplicate = firstDuplicate(under: world.container) {
                    return "After step \(index), node \(duplicate.tag)#\(duplicate.serial) "
                        + "appears in the tree more than once."
                }
            }
            return nil
        }
    }

    /// Every effect that ran must eventually be cleaned up — across reorders,
    /// type swaps and deletions, not just on a tidy unmount.
    @Test("effect cleanups balance effect runs")
    func effectBalance() {
        forAll("every mounted effect is cleaned up") { scenario in
            let world = World()
            for shape in scenario.steps {
                world.render(shape)
            }
            world.renderer.unmountAll()

            guard world.ledger.mounts != world.ledger.cleanups else { return nil }
            return """
                \(world.ledger.mounts) effect(s) ran but \(world.ledger.cleanups) \
                cleanup(s) fired; \(world.ledger.mounts - world.ledger.cleanups) leaked.
                """
        }
    }

    /// Duplicate sibling keys have no coherent meaning, so the reconciler
    /// refuses them rather than resolving the ambiguity arbitrarily. Found by
    /// the `idempotence` property above, which shrank a 22-node scenario down
    /// to two siblings sharing one key.
    @Test("two siblings sharing a key is a hard error, not undefined behaviour")
    func duplicateKeysTrap() async {
        await #expect(processExitsWith: .failure) {
            await MainActor.run {
                let host = TestHost()
                let container = host.makeContainer()
                let renderer = Reconciler(host: host, container: container)
                renderer.render(
                    Node("list") {
                        Node("row", key: "same")
                        Node("row", key: "same")
                    }
                )
            }
        }
    }

    /// Cleanups must not run early either — an effect torn down while its
    /// component is still mounted is just as wrong as one that leaks.
    @Test("no cleanup runs while its component is still mounted")
    func noPrematureCleanup() {
        forAll("cleanups never outpace mounts") { scenario in
            let world = World()
            for (index, shape) in scenario.steps.enumerated() {
                world.render(shape)
                if world.ledger.cleanups > world.ledger.mounts {
                    return "After step \(index): \(world.ledger.cleanups) cleanups against "
                        + "\(world.ledger.mounts) mounts."
                }
            }
            return nil
        }
    }
}

// MARK: - Properties over keyed lists

/// These generate a list of unique names and a permutation of it rather than an
/// arbitrary tree. The inputs are already small enough to read, so they run
/// without the shrinker.
@Suite("Keyed list properties")
@MainActor
struct KeyedListPropertyTests {
    private func makeList(_ names: [String]) -> (World, StateRegistry) {
        let world = World()
        let registry = StateRegistry()
        world.renderer.render(KeyedList(names: names, registry: registry).asElement())
        return (world, registry)
    }

    private func names(in world: World) -> [String] {
        world.container.children.first?.children.compactMap { node in
            guard case .string(let name)? = node.props["name"] else { return nil }
            return name
        } ?? []
    }

    @Test("any permutation preserves every child's state and creates nothing")
    func permutationPreservesState() {
        var seeds = SplitMix64(seed: propertySeed &* 3)

        for iteration in 0..<propertyCaseCount {
            let seed = seeds.next()
            var rng = SplitMix64(seed: seed)
            let context = "iteration \(iteration), seed \(seed)"

            let count = Int.random(in: 1...6, using: &rng)
            let names = (0..<count).map { "n\($0)" }
            let (world, registry) = makeList(names)

            for name in names where Bool.random(using: &rng) {
                registry.bump(name)
            }
            let before = registry.values

            let permuted = names.shuffled(using: &rng)
            world.clearLog()
            world.renderer.render(KeyedList(names: permuted, registry: registry).asElement())

            #expect(registry.values == before, "state must follow keys — \(context)")
            #expect(self.names(in: world) == permuted, "order must match — \(context)")
            #expect(
                world.log.allSatisfy { !$0.hasPrefix("create") && !$0.hasPrefix("remove") },
                "a permutation must only move nodes — \(context): \(world.log)"
            )
        }
    }

    @Test("inserting one child is exactly one placement and no moves")
    func insertionIsMinimal() {
        var seeds = SplitMix64(seed: propertySeed &* 5)

        for iteration in 0..<propertyCaseCount {
            let seed = seeds.next()
            var rng = SplitMix64(seed: seed)
            let context = "iteration \(iteration), seed \(seed)"

            let count = Int.random(in: 1...6, using: &rng)
            let names = (0..<count).map { "n\($0)" }
            let (world, registry) = makeList(names)

            var extended = names
            extended.insert("new", at: Int.random(in: 0...count, using: &rng))

            world.clearLog()
            world.renderer.render(KeyedList(names: extended, registry: registry).asElement())

            let placements = world.log.filter {
                $0.hasPrefix("insert") || $0.hasPrefix("move")
            }
            #expect(placements.count == 1, "one insertion, one placement — \(context)")
            #expect(
                world.log.allSatisfy { !$0.hasPrefix("move") },
                "nothing already in order should move — \(context): \(world.log)"
            )
            #expect(self.names(in: world) == extended, "order must match — \(context)")
        }
    }

    @Test("removing one child is exactly one removal and no moves")
    func removalIsMinimal() {
        var seeds = SplitMix64(seed: propertySeed &* 7)

        for iteration in 0..<propertyCaseCount {
            let seed = seeds.next()
            var rng = SplitMix64(seed: seed)
            let context = "iteration \(iteration), seed \(seed)"

            let count = Int.random(in: 2...6, using: &rng)
            let names = (0..<count).map { "n\($0)" }
            let (world, registry) = makeList(names)

            var reduced = names
            reduced.remove(at: Int.random(in: 0..<count, using: &rng))

            world.clearLog()
            world.renderer.render(KeyedList(names: reduced, registry: registry).asElement())

            #expect(
                world.log.filter { $0.hasPrefix("remove") }.count == 1,
                "one deletion, one removal — \(context): \(world.log)"
            )
            #expect(
                world.log.allSatisfy { !$0.hasPrefix("move") },
                "the survivors keep their order — \(context): \(world.log)"
            )
            #expect(self.names(in: world) == reduced, "order must match — \(context)")
        }
    }
}
