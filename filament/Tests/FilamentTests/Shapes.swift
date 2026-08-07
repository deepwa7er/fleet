import Filament

/// A generated element tree, in a form that can be compared, printed and
/// shrunk. `Element` itself holds closures and existentials, so it can do none
/// of those things — the shape is the value the generator actually manipulates.
indirect enum Shape {
    case text(String)
    case host(tag: String, key: String?, props: [String: String], children: [Shape])
    case component(id: Int, key: String?, child: Shape)
    case fragment(key: String?, children: [Shape])

    var key: String? {
        switch self {
        case .text: nil
        case .host(_, let key, _, _): key
        case .component(_, let key, _): key
        case .fragment(let key, _): key
        }
    }

    /// Text nodes have no key in the element model, so keying one is a no-op —
    /// which is itself worth generating, since it produces sibling lists that
    /// mix keyed and unkeyed children.
    func withKey(_ key: String?) -> Shape {
        switch self {
        case .text: self
        case .host(let tag, _, let props, let children):
            .host(tag: tag, key: key, props: props, children: children)
        case .component(let id, _, let child):
            .component(id: id, key: key, child: child)
        case .fragment(_, let children):
            .fragment(key: key, children: children)
        }
    }

    var nodeCount: Int {
        switch self {
        case .text: 1
        case .host(_, _, _, let children): 1 + children.reduce(0) { $0 + $1.nodeCount }
        case .component(_, _, let child): 1 + child.nodeCount
        case .fragment(_, let children): 1 + children.reduce(0) { $0 + $1.nodeCount }
        }
    }

    @MainActor
    func element(ledger: Ledger) -> Element {
        switch self {
        case .text(let value):
            return .text(value)

        case .host(let tag, let key, let props, let children):
            return .host(
                HostElement(
                    tag: tag,
                    props: Props(props.mapValues { Props.Value.string($0) }),
                    children: children.map { $0.element(ledger: ledger) },
                    key: key.map { AnyHashable($0) }
                )
            )

        case .component(let id, let key, let child):
            return Cell(id: id, child: child, ledger: ledger)
                .asElement(key: key.map { AnyHashable($0) })

        case .fragment(let key, let children):
            return .fragment(
                FragmentElement(
                    children: children.map { $0.element(ledger: ledger) },
                    key: key.map { AnyHashable($0) }
                )
            )
        }
    }

    func describe(indent: Int = 0) -> String {
        let pad = String(repeating: "  ", count: indent)
        switch self {
        case .text(let value):
            return "\(pad)\"\(value)\""

        case .host(let tag, let key, let props, let children):
            let keyLabel = key.map { " key=\($0)" } ?? ""
            let propLabel = props.isEmpty
                ? ""
                : " " + props.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }
                    .joined(separator: " ")
            var line = "\(pad)<\(tag)\(keyLabel)\(propLabel)>"
            for child in children { line += "\n" + child.describe(indent: indent + 1) }
            return line

        case .component(let id, let key, let child):
            let keyLabel = key.map { " key=\($0)" } ?? ""
            return "\(pad)Cell#\(id)\(keyLabel)\n" + child.describe(indent: indent + 1)

        case .fragment(let key, let children):
            let keyLabel = key.map { " key=\($0)" } ?? ""
            var line = "\(pad)<>\(keyLabel)"
            for child in children { line += "\n" + child.describe(indent: indent + 1) }
            return line
        }
    }
}

// MARK: - Generation

private let tagPool = ["box", "row", "col", "span"]
private let wordPool = ["alpha", "beta", "gamma", "delta"]
private let propKeyPool = ["a", "b", "c"]
private let propValuePool = ["1", "2", "3"]

private let keyPool = ["k0", "k1", "k2", "k3", "k4"]

/// A key not already claimed by one of `siblings`.
///
/// Duplicate sibling keys are a caller error the reconciler traps on, so the
/// generator must never produce them — that case is covered by a dedicated exit
/// test instead.
private func unusedKey(among siblings: [Shape], using rng: inout SplitMix64) -> String? {
    let taken = Set(siblings.compactMap(\.key))
    let available = keyPool.filter { !taken.contains($0) }
    return available.randomElement(using: &rng)
}

extension Shape {
    /// The root of a scenario, always a container with at least two children.
    ///
    /// `randomTree` returns a bare text or component node 40% of the time, and
    /// a root like that has nowhere for a reorder, insertion or deletion to
    /// happen — every mutation degenerates into a prop or text change. Real UI
    /// roots are containers anyway, and the diversity lost here is recovered
    /// deeper in the tree where `randomTree` still runs unconstrained.
    static func randomRoot(using rng: inout SplitMix64) -> Shape {
        var children: [Shape] = []
        for _ in 0..<Int.random(in: 2...4, using: &rng) {
            var child = randomTree(depth: 1, using: &rng)
            if Int.random(in: 0..<10, using: &rng) < 7,
               let key = unusedKey(among: children, using: &rng) {
                child = child.withKey(key)
            }
            children.append(child)
        }
        return .host(
            tag: tagPool.randomElement(using: &rng)!,
            key: nil,
            props: [:],
            children: children
        )
    }

    static func randomTree(depth: Int = 0, using rng: inout SplitMix64) -> Shape {
        let roll = Int.random(in: 0..<10, using: &rng)

        if depth >= 3 || roll < 2 {
            return .text(wordPool.randomElement(using: &rng)!)
        }

        if roll < 4 {
            return .component(
                id: Int.random(in: 0..<100, using: &rng),
                key: nil,
                child: randomTree(depth: depth + 1, using: &rng)
            )
        }

        if roll == 4 {
            // Fragment children are siblings within the fragment, so key
            // uniqueness is scoped to this list — two sibling fragments may
            // each hold a "k0" without clashing.
            var children: [Shape] = []
            for _ in 0..<Int.random(in: 0..<3, using: &rng) {
                var child = randomTree(depth: depth + 1, using: &rng)
                if Int.random(in: 0..<10, using: &rng) < 7,
                   let key = unusedKey(among: children, using: &rng) {
                    child = child.withKey(key)
                }
                children.append(child)
            }
            return .fragment(key: nil, children: children)
        }

        let childCount = Int.random(in: 0..<5, using: &rng)
        var children: [Shape] = []
        for _ in 0..<childCount {
            var child = randomTree(depth: depth + 1, using: &rng)
            // Biased towards keyed, because moves only happen among keyed
            // children and moves are the part of the reconciler most worth
            // hammering. Measured by GeneratorCoverageTests.
            if Int.random(in: 0..<10, using: &rng) < 7,
               let key = unusedKey(among: children, using: &rng) {
                child = child.withKey(key)
            }
            children.append(child)
        }

        var props: [String: String] = [:]
        for propKey in propKeyPool where Bool.random(using: &rng) {
            props[propKey] = propValuePool.randomElement(using: &rng)!
        }

        return .host(
            tag: tagPool.randomElement(using: &rng)!,
            key: nil,
            props: props,
            children: children
        )
    }

    /// Produces the next step of a scenario by perturbing this one.
    ///
    /// Steps are related rather than independently random on purpose: a fresh
    /// tree every step would remount everything and never exercise a move, an
    /// in-place prop update, or state surviving a reorder — which is most of
    /// what there is to get wrong.
    func mutated(using rng: inout SplitMix64) -> Shape {
        switch self {
        case .text:
            return .text(wordPool.randomElement(using: &rng)!)

        case .component(let id, let key, let child):
            return .component(id: id, key: key, child: child.mutated(using: &rng))

        case .fragment(let key, var children):
            switch Int.random(in: 0..<4, using: &rng) {
            case 0:
                children.shuffle(using: &rng)
            case 1 where !children.isEmpty:
                children.remove(at: Int.random(in: 0..<children.count, using: &rng))
            case 2:
                var inserted = Shape.randomTree(depth: 2, using: &rng)
                if let key = unusedKey(among: children, using: &rng) {
                    inserted = inserted.withKey(key)
                }
                children.insert(inserted, at: Int.random(in: 0...children.count, using: &rng))
            case 3 where !children.isEmpty:
                let index = Int.random(in: 0..<children.count, using: &rng)
                children[index] = children[index].mutated(using: &rng)
            default:
                break
            }
            return .fragment(key: key, children: children)

        case .host(let tag, let key, var props, var children):
            switch Int.random(in: 0..<10, using: &rng) {
            case 0:
                children.shuffle(using: &rng)

            case 8 where children.count > 1:
                // Rotation is the worst case for the `lastPlacedIndex`
                // heuristic — moving one child to the front reports every other
                // child as moved. Correct, but maximally busy, so worth hitting.
                children.insert(children.removeLast(), at: 0)

            case 9 where children.count > 1:
                let first = Int.random(in: 0..<children.count, using: &rng)
                let second = Int.random(in: 0..<children.count, using: &rng)
                children.swapAt(first, second)

            case 1 where !children.isEmpty:
                children.remove(at: Int.random(in: 0..<children.count, using: &rng))

            case 2:
                let position = Int.random(in: 0...children.count, using: &rng)
                var inserted = Shape.randomTree(depth: 2, using: &rng)
                if Bool.random(using: &rng),
                   let key = unusedKey(among: children, using: &rng) {
                    inserted = inserted.withKey(key)
                }
                children.insert(inserted, at: position)

            case 3:
                props[propKeyPool.randomElement(using: &rng)!] =
                    propValuePool.randomElement(using: &rng)!

            case 4 where !props.isEmpty:
                props.removeValue(forKey: props.keys.randomElement(using: &rng)!)

            case 5 where !children.isEmpty:
                // Flip a child between keyed and unkeyed, which is the case a
                // hand-written test is least likely to think of.
                let index = Int.random(in: 0..<children.count, using: &rng)
                var others = children
                others.remove(at: index)
                let newKey = children[index].key == nil
                    ? unusedKey(among: others, using: &rng)
                    : nil
                children[index] = children[index].withKey(newKey)

            case 6 where !children.isEmpty:
                let index = Int.random(in: 0..<children.count, using: &rng)
                children[index] = children[index].mutated(using: &rng)

            case 7 where !children.isEmpty:
                // Replace a child outright, usually changing its type and so
                // forcing a teardown rather than an update. The key is carried
                // over so this stays distinct from case 2's fresh insertion.
                let index = Int.random(in: 0..<children.count, using: &rng)
                let preservedKey = children[index].key
                children[index] = Shape.randomTree(depth: 2, using: &rng)
                    .withKey(preservedKey)

            default:
                break
            }

            return .host(tag: tag, key: key, props: props, children: children)
        }
    }
}

// MARK: - Structural queries, for measuring generator coverage

extension Shape {
    /// This node and every node beneath it.
    var allNodes: [Shape] {
        switch self {
        case .text:
            return [self]
        case .host(_, _, _, let children):
            return [self] + children.flatMap(\.allNodes)
        case .component(_, _, let child):
            return [self] + child.allNodes
        case .fragment(_, let children):
            return [self] + children.flatMap(\.allNodes)
        }
    }

    var childShapes: [Shape] {
        switch self {
        case .text: []
        case .host(_, _, _, let children): children
        case .component(_, _, let child): [child]
        case .fragment(_, let children): children
        }
    }

    var isComponent: Bool {
        if case .component = self { return true }
        return false
    }

    var isFragment: Bool {
        if case .fragment = self { return true }
        return false
    }

    var containsFragment: Bool { allNodes.contains(where: \.isFragment) }

    /// A fragment holding more than one child, which is the case that actually
    /// exercises flattening rather than behaving like a plain wrapper.
    var containsMultiChildFragment: Bool {
        allNodes.contains { $0.isFragment && $0.childShapes.count > 1 }
    }

    var containsComponent: Bool { allNodes.contains(where: \.isComponent) }

    /// A component whose immediate output is another component, so the host
    /// nodes it must place are two levels of indirection away.
    var containsNestedComponent: Bool {
        allNodes.contains { node in
            if case .component(_, _, let child) = node { return child.isComponent }
            return false
        }
    }

    /// A sibling list holding both keyed and unkeyed children — the case where
    /// key-matching and position-matching have to coexist.
    var hasMixedKeyedSiblings: Bool {
        allNodes.contains { node in
            let children = node.childShapes
            guard children.count > 1 else { return false }
            return children.contains { $0.key != nil } && children.contains { $0.key == nil }
        }
    }

    var hasKeyedSiblingList: Bool {
        allNodes.contains { $0.childShapes.count(where: { $0.key != nil }) > 1 }
    }

    var hasKeyedComponentChild: Bool {
        allNodes.contains { $0.childShapes.contains { $0.key != nil && $0.isComponent } }
    }
}

// MARK: - Shrinking

extension Shape {
    /// Strictly smaller variants of this shape, cheapest reduction first.
    func shrinkCandidates() -> [Shape] {
        var candidates: [Shape] = []

        switch self {
        case .text(let value):
            if value != wordPool[0] { candidates.append(.text(wordPool[0])) }

        case .component(_, let key, let child):
            // Collapsing a component to its child removes a whole indirection.
            candidates.append(child)
            if key != nil { candidates.append(withKey(nil)) }
            candidates.append(contentsOf: child.shrinkCandidates().map {
                if case .component(let id, let key, _) = self {
                    return Shape.component(id: id, key: key, child: $0)
                }
                return $0
            })

        case .fragment(let key, let children):
            candidates.append(contentsOf: children)
            for index in children.indices {
                var reduced = children
                reduced.remove(at: index)
                candidates.append(.fragment(key: key, children: reduced))
            }
            if key != nil { candidates.append(withKey(nil)) }
            for index in children.indices {
                for reducedChild in children[index].shrinkCandidates() {
                    var reduced = children
                    reduced[index] = reducedChild
                    candidates.append(.fragment(key: key, children: reduced))
                }
            }

        case .host(let tag, let key, let props, let children):
            candidates.append(contentsOf: children)

            for index in children.indices {
                var reduced = children
                reduced.remove(at: index)
                candidates.append(.host(tag: tag, key: key, props: props, children: reduced))
            }

            if key != nil { candidates.append(withKey(nil)) }

            if !props.isEmpty {
                candidates.append(.host(tag: tag, key: key, props: [:], children: children))
            }

            for index in children.indices {
                for reducedChild in children[index].shrinkCandidates() {
                    var reduced = children
                    reduced[index] = reducedChild
                    candidates.append(.host(tag: tag, key: key, props: props, children: reduced))
                }
            }
        }

        return candidates
    }
}

// MARK: - Scenarios

/// A sequence of trees rendered into one reconciler, in order.
struct Scenario {
    var steps: [Shape]

    var nodeCount: Int { steps.reduce(0) { $0 + $1.nodeCount } }

    var description: String {
        steps.enumerated()
            .map { "  step \($0.offset):\n" + $0.element.describe(indent: 2) }
            .joined(separator: "\n")
    }

    static func random(using rng: inout SplitMix64) -> Scenario {
        var shape = Shape.randomRoot(using: &rng)
        var steps = [shape]

        for _ in 0..<Int.random(in: 1...4, using: &rng) {
            shape = shape.mutated(using: &rng)
            steps.append(shape)
        }

        return Scenario(steps: steps)
    }

    func shrinkCandidates() -> [Scenario] {
        var candidates: [Scenario] = []

        if steps.count > 1 {
            for index in steps.indices {
                var reduced = steps
                reduced.remove(at: index)
                candidates.append(Scenario(steps: reduced))
            }
        }

        for index in steps.indices {
            for reducedShape in steps[index].shrinkCandidates() {
                var reduced = steps
                reduced[index] = reducedShape
                candidates.append(Scenario(steps: reduced))
            }
        }

        return candidates
    }
}

// MARK: - Instrumented component

/// Counts effect mounts against cleanups, so an imbalance is detectable no
/// matter which fiber it happened on.
@MainActor
final class Ledger {
    var mounts = 0
    var cleanups = 0
}

/// A stateful component the generator can nest anywhere.
struct Cell: Component {
    let id: Int
    let child: Shape
    let ledger: Ledger

    func render() -> Element {
        useEffect([]) {
            ledger.mounts += 1
            return { ledger.cleanups += 1 }
        }
        return child.element(ledger: ledger)
    }
}

// MARK: - Keyed-list fixtures

@MainActor
final class StateRegistry {
    var values: [String: Int] = [:]
    var setters: [String: Setter<Int>] = [:]

    func bump(_ name: String) {
        guard let setter = setters[name] else { return }
        setter { $0 + 1 }
    }
}

struct KeyedCell: Component {
    let name: String
    let registry: StateRegistry

    func render() -> Element {
        let (value, setValue) = useState(0)
        registry.values[name] = value
        registry.setters[name] = setValue
        return Node("cell", ["name": .string(name), "value": .number(Double(value))])
    }
}

struct KeyedList: Component {
    let names: [String]
    let registry: StateRegistry

    func render() -> Element {
        Node("list") {
            for name in names {
                Keyed(name, KeyedCell(name: name, registry: registry))
            }
        }
    }
}
