import Testing
@testable import Filament

@MainActor
private func makeRenderer() -> (host: TestHost, container: TestNode, renderer: Reconciler<TestHost>) {
    let host = TestHost()
    let container = host.makeContainer()
    return (host, container, Reconciler(host: host, container: container))
}

// MARK: - Rendering

@Suite("Rendering")
@MainActor
struct RenderingTests {
    @Test("builds the described tree")
    func initialRender() {
        let (_, container, renderer) = makeRenderer()

        renderer.render(
            Node("app", ["title": .string("hello")]) {
                Node("row") {
                    "text child"
                }
                Node("row", ["muted": .bool(true)])
            }
        )

        #expect(container.describe() == """
        <#root>
          <app title="hello">
            <row>
              "text child"
            <row muted=true>
        """)
    }

    @Test("a bare string in a tree becomes a text node")
    func textChildren() {
        let (_, container, renderer) = makeRenderer()
        renderer.render(Node("label") { "hi" })

        #expect(container.children[0].children[0].text == "hi")
        #expect(container.children[0].children[0].tag == "#text")
    }

    @Test("optionals and loops in the builder shape the child list")
    func controlFlowInBuilder() {
        let (_, container, renderer) = makeRenderer()
        let show = true

        renderer.render(
            Node("app") {
                if show {
                    Node("shown")
                }
                for index in 0..<3 {
                    Node("item", ["i": .number(Double(index))])
                }
            }
        )

        let tags = container.children[0].children.map(\.tag)
        #expect(tags == ["shown", "item", "item", "item"])
    }
}

// MARK: - Diffing

@Suite("Diffing")
@MainActor
struct DiffingTests {
    @Test("a state change updates the existing host node instead of replacing it")
    func updatesInPlace() {
        let (host, container, renderer) = makeRenderer()
        let probe = Probe()

        renderer.render(Counter(probe: probe, label: "x").asElement())
        let node = container.children[0]
        host.clearLog()

        probe.bump()

        #expect(container.children[0] === node, "the host node must survive the update")
        #expect(host.log == ["update counter#1 +count"])
        #expect(probe.count == 1)
    }

    @Test("only changed props are sent to the host")
    func minimalPropUpdate() {
        let (host, _, renderer) = makeRenderer()

        renderer.render(Node("box", ["a": .string("1"), "b": .string("2")]))
        host.clearLog()
        renderer.render(Node("box", ["a": .string("1"), "b": .string("changed")]))

        #expect(host.log == ["update box#1 +b"])
    }

    @Test("a removed prop is reported as removed")
    func removedProp() {
        let (host, _, renderer) = makeRenderer()

        renderer.render(Node("box", ["a": .string("1"), "b": .string("2")]))
        host.clearLog()
        renderer.render(Node("box", ["a": .string("1")]))

        #expect(host.log == ["update box#1 -b"])
    }

    @Test("an unchanged tree produces no host mutations at all")
    func noChangeNoWork() {
        let (host, _, renderer) = makeRenderer()

        renderer.render(Node("app") { Node("child", ["id": .string("a")]) })
        host.clearLog()
        renderer.render(Node("app") { Node("child", ["id": .string("a")]) })

        #expect(host.log.isEmpty)
    }

    @Test("a handler prop is re-bound every render, because closures cannot be compared")
    func handlersAlwaysCountAsChanged() {
        let (host, _, renderer) = makeRenderer()
        func tree() -> Element { Node("box", ["onTap": .handler {}, "id": .string("a")]) }

        renderer.render(tree())
        host.clearLog()
        renderer.render(tree())

        // `id` is correctly recognised as unchanged; `onTap` cannot be, so any
        // node carrying a handler reports an update on every single render.
        // React sidesteps this with root-level event delegation rather than by
        // solving the comparison problem, which is not solvable.
        #expect(host.log == ["update box#1 +onTap"])
    }

    @Test("changing the element type tears the subtree down and resets its state")
    func typeChangeResetsState() {
        let (host, container, renderer) = makeRenderer()
        let probe = Probe()

        renderer.render(Swapper(showCounter: true, probe: probe).asElement())
        probe.bump()
        #expect(probe.count == 1)

        host.clearLog()
        renderer.render(Swapper(showCounter: false, probe: probe).asElement())
        #expect(container.children[0].children.map(\.tag) == ["placeholder"])
        #expect(host.log.contains { $0.hasPrefix("remove counter") })

        renderer.render(Swapper(showCounter: true, probe: probe).asElement())
        #expect(probe.count == 0, "a rebuilt subtree must not inherit the old state")
    }

    @Test("removing a child detaches its host node")
    func removal() {
        let (_, container, renderer) = makeRenderer()

        renderer.render(Node("app") { Node("a"); Node("b") })
        #expect(container.children[0].children.map(\.tag) == ["a", "b"])

        renderer.render(Node("app") { Node("a") })
        #expect(container.children[0].children.map(\.tag) == ["a"])
    }
}

// MARK: - Keys

@Suite("Keys")
@MainActor
struct KeyTests {
    private func probes(_ names: [String]) -> [String: Probe] {
        Dictionary(uniqueKeysWithValues: names.map { ($0, Probe()) })
    }

    @Test("keyed children carry their state through a reorder")
    func keyedReorderPreservesState() {
        let (host, container, renderer) = makeRenderer()
        let names = ["A", "B", "C"]
        let probes = probes(names)

        renderer.render(List(names: names, probes: probes, keyed: true).asElement())
        probes["A"]!.bump()
        #expect(probes["A"]!.count == 1)

        host.clearLog()
        renderer.render(List(names: ["B", "C", "A"], probes: probes, keyed: true).asElement())

        #expect(probes["A"]!.count == 1, "state must follow the key, not the slot")
        #expect(probes["B"]!.count == 0)
        #expect(host.log.allSatisfy { !$0.hasPrefix("create") }, "a reorder must not rebuild")

        let labels = container.children[0].children.map { node -> String in
            guard case .string(let label)? = node.props["label"] else { return "?" }
            return label
        }
        #expect(labels == ["B", "C", "A"])
    }

    @Test("unkeyed children keep the state of the slot they land in")
    func unkeyedReorderLosesState() {
        let (_, _, renderer) = makeRenderer()
        let names = ["A", "B", "C"]
        let probes = probes(names)

        renderer.render(List(names: names, probes: probes, keyed: false).asElement())
        probes["A"]!.bump()

        renderer.render(List(names: ["B", "C", "A"], probes: probes, keyed: false).asElement())

        // Position 0 kept its state and is now rendering as "B". This is the
        // bug keys exist to prevent, asserted here so the contrast is explicit.
        #expect(probes["B"]!.count == 1)
        #expect(probes["A"]!.count == 0)
    }

    @Test("inserting at the head moves nothing that was already in order")
    func headInsertionIsOneMutation() {
        let (host, container, renderer) = makeRenderer()
        let probes = probes(["A", "B", "C"])

        renderer.render(List(names: ["B", "C"], probes: probes, keyed: true).asElement())
        host.clearLog()
        renderer.render(List(names: ["A", "B", "C"], probes: probes, keyed: true).asElement())

        let placements = host.log.filter { $0.hasPrefix("insert") || $0.hasPrefix("move") }
        #expect(placements.count == 1, "only the new child should be positioned")
        #expect(placements[0].hasPrefix("insert"))
        #expect(host.log.filter { $0.hasPrefix("move") }.isEmpty)

        let labels = container.children[0].children.map { node -> String in
            guard case .string(let label)? = node.props["label"] else { return "?" }
            return label
        }
        #expect(labels == ["A", "B", "C"])
    }

    @Test("a moved component drags the host nodes it does not own")
    func moveThroughNestedComponents() {
        let (host, container, renderer) = makeRenderer()
        let probes = probes(["A", "B", "C"])

        renderer.render(WrapperList(names: ["A", "B", "C"], probes: probes).asElement())
        probes["C"]!.bump()
        host.clearLog()

        renderer.render(WrapperList(names: ["C", "A", "B"], probes: probes).asElement())

        let labels = container.children[0].children.map { node -> String in
            guard case .string(let label)? = node.props["label"] else { return "?" }
            return label
        }
        #expect(labels == ["C", "A", "B"])
        #expect(probes["C"]!.count == 1)
        #expect(host.log.allSatisfy { !$0.hasPrefix("create") })
    }
}

// MARK: - Hooks

@Suite("Hooks")
@MainActor
struct HookTests {
    @Test("sibling components hold independent state")
    func independentState() {
        let (_, _, renderer) = makeRenderer()
        let left = Probe()
        let right = Probe()

        renderer.render(
            Node("app") {
                Counter(probe: left, label: "left")
                Counter(probe: right, label: "right")
            }
        )

        left.bump()
        left.bump()

        #expect(left.count == 2)
        #expect(right.count == 0)
    }

    @Test("the updater form sees the live value, the direct form sees the snapshot")
    func updaterVersusValue() {
        let (_, _, renderer) = makeRenderer()
        let probe = Probe()
        renderer.render(Counter(probe: probe, label: "x").asElement())

        let setter = probe.setCount!
        renderer.batch {
            setter { $0 + 1 }
            setter { $0 + 1 }
        }
        #expect(probe.count == 2, "updaters compose")

        let snapshot = probe.count
        renderer.batch {
            setter(snapshot + 1)
            setter(snapshot + 1)
        }
        #expect(probe.count == 3, "direct sets from one snapshot collapse into one")
    }

    @Test("updates inside a batch produce a single render")
    func batching() {
        let (_, _, renderer) = makeRenderer()
        let probe = PairProbe()
        renderer.render(Pair(probe: probe).asElement())
        let baseline = probe.renders

        renderer.batch {
            probe.setA!(1)
            probe.setB!(2)
        }

        #expect(probe.renders == baseline + 1)
        #expect(probe.a == 1)
        #expect(probe.b == 2)
    }

    @Test("unbatched updates render once each")
    func unbatched() {
        let (_, _, renderer) = makeRenderer()
        let probe = PairProbe()
        renderer.render(Pair(probe: probe).asElement())
        let baseline = probe.renders

        probe.setA!(1)
        probe.setB!(2)

        #expect(probe.renders == baseline + 2)
    }

    @Test("effects run on mount, re-run when dependencies change, and clean up")
    func effectLifecycle() {
        let (_, _, renderer) = makeRenderer()
        let probe = EffectProbe()

        renderer.render(Effectful(probe: probe, value: 1, watchValue: true).asElement())
        #expect(probe.events == ["run 1"])

        renderer.render(Effectful(probe: probe, value: 1, watchValue: true).asElement())
        #expect(probe.events == ["run 1"], "unchanged dependencies must not re-run")

        renderer.render(Effectful(probe: probe, value: 2, watchValue: true).asElement())
        #expect(probe.events == ["run 1", "cleanup 1", "run 2"])

        renderer.unmountAll()
        #expect(probe.events == ["run 1", "cleanup 1", "run 2", "cleanup 2"])
    }

    @Test("empty dependencies run the effect exactly once")
    func mountOnlyEffect() {
        let (_, _, renderer) = makeRenderer()
        let probe = EffectProbe()

        renderer.render(Effectful(probe: probe, value: 1, watchValue: false).asElement())
        renderer.render(Effectful(probe: probe, value: 2, watchValue: false).asElement())
        renderer.render(Effectful(probe: probe, value: 3, watchValue: false).asElement())

        #expect(probe.events == ["run 1"])
    }

    @Test("effects observe the committed host tree, not the tree mid-update")
    func effectsRunAfterCommit() {
        let (_, container, renderer) = makeRenderer()

        final class Witness { var tagsAtEffectTime: [String] = [] }
        let witness = Witness()

        struct Observer: Component {
            let container: TestNode
            let witness: Witness

            func render() -> Element {
                useEffect(nil) {
                    witness.tagsAtEffectTime = container.children.map(\.tag)
                }
                return Node("committed")
            }
        }

        renderer.render(Observer(container: container, witness: witness).asElement())
        #expect(witness.tagsAtEffectTime == ["committed"])
    }
}
