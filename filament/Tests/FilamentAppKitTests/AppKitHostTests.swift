import AppKit
import Testing
import Filament
import FilamentAppKit

// MARK: - Stand-ins for an app's own hand-written views

/// Plays the part of Tiler's `Chip`: draws itself, owns its own hover and click
/// behaviour, and accepts a small set of props. Nothing about it is generated —
/// which is the point, since a real app's leaf views stay exactly as they are.
@MainActor
final class ProbeChip: NSView, PropApplying {
    private(set) var title = ""
    private(set) var isSelected = false
    private(set) var applyCount = 0
    private var onClick: (@MainActor () -> Void)?

    init(props: Props) {
        super.init(frame: .zero)
        for (key, value) in props.storage { assign(key, value) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    func applyProps(updated: [String: Props.Value], removed: [String]) {
        applyCount += 1
        for (key, value) in updated { assign(key, value) }
        for key in removed { clear(key) }
    }

    /// Stands in for a real click landing on this view.
    func click() { onClick?() }

    private func assign(_ key: String, _ value: Props.Value) {
        switch (key, value) {
        case ("title", .string(let text)): title = text
        case ("selected", .bool(let flag)): isSelected = flag
        case ("onClick", .handler(let action)): onClick = action
        default: break
        }
    }

    private func clear(_ key: String) {
        switch key {
        case "title": title = ""
        case "selected": isSelected = false
        case "onClick": onClick = nil
        default: break
        }
    }
}

/// A plain container, standing in for the grouping views a panel needs.
@MainActor
final class ProbeContainer: NSView, PropApplying {
    func applyProps(updated: [String: Props.Value], removed: [String]) {}
}

/// A registered view that cannot accept prop changes, for the trap test.
@MainActor
final class InertView: NSView {}

// MARK: - Harness

@MainActor
final class Stats {
    var created = 0
}

@MainActor
private func makeHost() -> (host: AppKitHost, root: NSView, stats: Stats) {
    let host = AppKitHost()
    let stats = Stats()
    host.register("chip") { props in
        stats.created += 1
        return ProbeChip(props: props)
    }
    // Containers are not counted, so `stats.created` stays a clean measure of
    // how many leaf views the reconciler had to build.
    host.register("row") { _ in ProbeContainer(frame: .zero) }
    return (host, NSView(frame: .zero), stats)
}

private extension NSView {
    var chips: [ProbeChip] { subviews.compactMap { $0 as? ProbeChip } }
    var titles: [String] { chips.map(\.title) }
}

// MARK: - Tests

@Suite("AppKit host")
@MainActor
struct AppKitHostTests {
    @Test("renders real NSViews into the container in tree order")
    func rendersInOrder() {
        let (host, root, stats) = makeHost()
        let renderer = Reconciler(host: host, container: root)

        renderer.render(
            Node("row") {
                Node("chip", ["title": .string("a")])
                Node("chip", ["title": .string("b")])
                Node("chip", ["title": .string("c")])
            }
        )

        #expect(root.subviews.count == 1, "the row is the only direct child")
        #expect(root.subviews[0].titles == ["a", "b", "c"])
        #expect(stats.created == 3)
    }

    @Test("a prop change updates the existing view instead of replacing it")
    func updatesInPlace() {
        let (host, root, stats) = makeHost()
        let renderer = Reconciler(host: host, container: root)

        renderer.render(Node("chip", ["title": .string("before")]))
        let view = root.chips[0]

        renderer.render(Node("chip", ["title": .string("after")]))

        #expect(root.chips[0] === view, "the NSView must survive the update")
        #expect(view.title == "after")
        #expect(view.applyCount == 1)
        #expect(stats.created == 1, "no new view should have been built")
    }

    @Test("an unchanged tree touches no view at all")
    func noChangeNoWork() {
        let (host, root, stats) = makeHost()
        let renderer = Reconciler(host: host, container: root)

        renderer.render(Node("chip", ["title": .string("steady")]))
        renderer.render(Node("chip", ["title": .string("steady")]))

        #expect(root.chips[0].applyCount == 0)
        #expect(stats.created == 1)
    }

    @Test("a keyed reorder moves the same views rather than rebuilding them")
    func keyedReorder() {
        let (host, root, stats) = makeHost()
        let renderer = Reconciler(host: host, container: root)

        func row(_ names: [String]) -> Element {
            Node("row") {
                for name in names {
                    Node("chip", key: name, ["title": .string(name)])
                }
            }
        }

        renderer.render(row(["a", "b", "c"]))
        let container = root.subviews[0]
        let original = Dictionary(
            uniqueKeysWithValues: container.chips.map { ($0.title, $0) }
        )

        renderer.render(row(["c", "a", "b"]))

        #expect(container.titles == ["c", "a", "b"])
        #expect(stats.created == 3, "a reorder must not create views")
        for chip in container.chips {
            #expect(original[chip.title] === chip, "\(chip.title) should be the same view")
        }
    }

    @Test("inserting at the head builds one view and leaves the rest in place")
    func headInsertion() {
        let (host, root, stats) = makeHost()
        let renderer = Reconciler(host: host, container: root)

        func row(_ names: [String]) -> Element {
            Node("row") {
                for name in names {
                    Node("chip", key: name, ["title": .string(name)])
                }
            }
        }

        renderer.render(row(["b", "c"]))
        let container = root.subviews[0]
        let existing = container.chips

        renderer.render(row(["a", "b", "c"]))

        #expect(container.titles == ["a", "b", "c"])
        #expect(stats.created == 3, "only the new chip is built")
        #expect(container.chips[1] === existing[0])
        #expect(container.chips[2] === existing[1])
    }

    @Test("a removed child is detached from its superview")
    func removalDetaches() {
        let (host, root, _) = makeHost()
        let renderer = Reconciler(host: host, container: root)

        renderer.render(
            Node("row") {
                Node("chip", key: "a", ["title": .string("a")])
                Node("chip", key: "b", ["title": .string("b")])
            }
        )
        let container = root.subviews[0]
        let doomed = container.chips[1]

        renderer.render(
            Node("row") {
                Node("chip", key: "a", ["title": .string("a")])
            }
        )

        #expect(container.titles == ["a"])
        #expect(doomed.superview == nil, "the detached view must leave the hierarchy")
    }

    @Test("a click handler drives component state and lands back on the view")
    func handlersRoundTrip() {
        struct Toggle: Component {
            func render() -> Element {
                let (on, setOn) = useState(false)
                return Node("chip", [
                    "title": .string(on ? "on" : "off"),
                    "onClick": .handler { setOn { !$0 } },
                ])
            }
        }

        let (host, root, stats) = makeHost()
        let renderer = Reconciler(host: host, container: root)
        renderer.render(Toggle().asElement())

        let chip = root.chips[0]
        #expect(chip.title == "off")

        chip.click()
        #expect(chip.title == "on")

        chip.click()
        #expect(chip.title == "off")
        #expect(stats.created == 1, "toggling state must not rebuild the view")
    }
}

// MARK: - The step-1 target: Tiler's chip rows

/// A direct stand-in for `CommandContentView`'s display and action chip rows,
/// which today are destroyed and rebuilt in full on every `reload()`.
@Suite("Chip row parity")
@MainActor
struct ChipRowTests {
    struct ChipRow: Component {
        let titles: [String]
        let selected: String?
        let onPick: @MainActor (String) -> Void

        func render() -> Element {
            Node("row") {
                for title in titles {
                    Node("chip", key: title, [
                        "title": .string(title),
                        "selected": .bool(title == selected),
                        "onClick": .handler { onPick(title) },
                    ])
                }
            }
        }
    }

    @Test("changing the selected chip rebuilds nothing")
    func selectionChange() {
        let (host, root, stats) = makeHost()
        let renderer = Reconciler(host: host, container: root)
        let displays = ["Built-in", "Studio Display", "Sidecar"]

        renderer.render(
            ChipRow(titles: displays, selected: "Built-in", onPick: { _ in }).asElement()
        )
        let container = root.subviews[0]
        let before = container.chips

        renderer.render(
            ChipRow(titles: displays, selected: "Sidecar", onPick: { _ in }).asElement()
        )

        #expect(stats.created == 3, "today this path recreates every chip; here, none")
        #expect(container.chips[0].isSelected == false)
        #expect(container.chips[2].isSelected == true)
        for (index, chip) in container.chips.enumerated() {
            #expect(chip === before[index], "chip \(index) must be the same NSView")
        }
    }

    /// Honest about a real cost. A handler prop can never diff as unchanged, so
    /// every chip carrying an `onClick` is asked to re-bind it on every render.
    /// That is a closure assignment, against today's cost of destroying and
    /// rebuilding an `NSView` — but it is not nothing, and it is why a real
    /// backend eventually wants React-style event delegation.
    @Test("handlers cost one cheap re-bind per chip per render")
    func handlerRebindCost() {
        let (host, root, _) = makeHost()
        let renderer = Reconciler(host: host, container: root)
        let displays = ["Built-in", "Studio Display"]

        renderer.render(
            ChipRow(titles: displays, selected: "Built-in", onPick: { _ in }).asElement()
        )
        renderer.render(
            ChipRow(titles: displays, selected: "Built-in", onPick: { _ in }).asElement()
        )

        let container = root.subviews[0]
        #expect(container.chips.allSatisfy { $0.applyCount == 1 })
    }

    @Test("a display appearing adds one chip and disturbs no other")
    func displayConnected() {
        let (host, root, stats) = makeHost()
        let renderer = Reconciler(host: host, container: root)

        renderer.render(
            ChipRow(titles: ["Built-in"], selected: "Built-in", onPick: { _ in }).asElement()
        )
        let container = root.subviews[0]
        let original = container.chips[0]

        renderer.render(
            ChipRow(
                titles: ["Built-in", "Studio Display"],
                selected: "Built-in",
                onPick: { _ in }
            ).asElement()
        )

        #expect(stats.created == 2, "one new chip, not a rebuilt row")
        #expect(container.chips[0] === original)
        #expect(container.titles == ["Built-in", "Studio Display"])
    }
}

// MARK: - Caller mistakes are loud

@Suite("AppKit host contract")
@MainActor
struct AppKitHostContractTests {
    @Test("an unregistered tag traps instead of rendering nothing")
    func unknownTagTraps() async {
        await #expect(processExitsWith: .failure) {
            await MainActor.run {
                let host = AppKitHost()
                let root = NSView(frame: .zero)
                Reconciler(host: host, container: root).render(Node("nope"))
            }
        }
    }

    @Test("a view that cannot accept prop changes traps rather than dropping them")
    func nonConformingViewTraps() async {
        await #expect(processExitsWith: .failure) {
            await MainActor.run {
                let host = AppKitHost()
                host.register("inert") { _ in InertView(frame: .zero) }
                let root = NSView(frame: .zero)
                let renderer = Reconciler(host: host, container: root)
                renderer.render(Node("inert", ["a": .string("1")]))
                renderer.render(Node("inert", ["a": .string("2")]))
            }
        }
    }

    @Test("a text node with no registered factory traps")
    func unregisteredTextTraps() async {
        await #expect(processExitsWith: .failure) {
            await MainActor.run {
                let host = AppKitHost()
                host.register("row") { _ in NSView(frame: .zero) }
                let root = NSView(frame: .zero)
                Reconciler(host: host, container: root).render(Node("row") { "hello" })
            }
        }
    }
}
