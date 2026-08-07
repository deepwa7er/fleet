import Testing
@testable import Filament

@Suite("Fragments")
@MainActor
struct FragmentTests {
    private func makeRenderer() -> (TestHost, TestNode, Reconciler<TestHost>) {
        let host = TestHost()
        let container = host.makeContainer()
        return (host, container, Reconciler(host: host, container: container))
    }

    @Test("a fragment's children land in the nearest host ancestor")
    func flattensIntoParent() {
        let (_, container, renderer) = makeRenderer()

        renderer.render(
            Node("app") {
                Node("a")
                Fragment {
                    Node("b")
                    Node("c")
                }
                Node("d")
            }
        )

        #expect(container.children[0].children.map(\.tag) == ["a", "b", "c", "d"])
    }

    @Test("a component can return a list rather than a single node")
    func componentReturningFragment() {
        struct Pair: Component {
            func render() -> Element {
                Fragment {
                    Node("left")
                    Node("right")
                }
            }
        }

        let (_, container, renderer) = makeRenderer()
        renderer.render(Node("app") { Node("before"); Pair(); Node("after") })

        #expect(
            container.children[0].children.map(\.tag) == ["before", "left", "right", "after"]
        )
    }

    @Test("nested fragments flatten all the way down")
    func nested() {
        let (_, container, renderer) = makeRenderer()

        renderer.render(
            Node("app") {
                Fragment {
                    Node("a")
                    Fragment {
                        Node("b")
                        Fragment { Node("c") }
                    }
                }
            }
        )

        #expect(container.children[0].children.map(\.tag) == ["a", "b", "c"])
    }

    @Test("a fragment growing inserts only the new child")
    func growth() {
        let (host, container, renderer) = makeRenderer()

        func tree(_ tags: [String]) -> Element {
            Node("app") {
                Node("head")
                Fragment {
                    for tag in tags { Node(tag, key: tag) }
                }
                Node("tail")
            }
        }

        renderer.render(tree(["a", "b"]))
        host.clearLog()
        renderer.render(tree(["a", "b", "c"]))

        #expect(container.children[0].children.map(\.tag) == ["head", "a", "b", "c", "tail"])
        #expect(host.log.filter { $0.hasPrefix("create") }.count == 1)
        #expect(host.log.filter { $0.hasPrefix("move") }.isEmpty)
    }

    @Test("a fragment inserted before existing siblings anchors correctly")
    func anchoring() {
        let (_, container, renderer) = makeRenderer()

        func tree(includeFragment: Bool) -> Element {
            Node("app") {
                if includeFragment {
                    Fragment {
                        Node("x")
                        Node("y")
                    }
                }
                Node("tail")
            }
        }

        renderer.render(tree(includeFragment: false))
        renderer.render(tree(includeFragment: true))

        #expect(container.children[0].children.map(\.tag) == ["x", "y", "tail"])
    }

    @Test("removing a fragment removes every child it contributed")
    func removal() {
        let (host, container, renderer) = makeRenderer()

        func tree(includeFragment: Bool) -> Element {
            Node("app") {
                Node("head")
                if includeFragment {
                    Fragment {
                        Node("x")
                        Node("y")
                    }
                }
            }
        }

        renderer.render(tree(includeFragment: true))
        host.clearLog()
        renderer.render(tree(includeFragment: false))

        #expect(container.children[0].children.map(\.tag) == ["head"])
        #expect(host.log.filter { $0.hasPrefix("remove") }.count == 2)
    }

    @Test("state inside a fragment survives a reorder of the fragment itself")
    func keyedFragmentReorder() {
        let (host, container, renderer) = makeRenderer()
        let left = Probe()
        let right = Probe()

        func tree(leftFirst: Bool) -> Element {
            Node("app") {
                if leftFirst {
                    Fragment(key: "L") { Counter(probe: left, label: "L") }
                    Fragment(key: "R") { Counter(probe: right, label: "R") }
                } else {
                    Fragment(key: "R") { Counter(probe: right, label: "R") }
                    Fragment(key: "L") { Counter(probe: left, label: "L") }
                }
            }
        }

        renderer.render(tree(leftFirst: true))
        left.bump()
        host.clearLog()

        renderer.render(tree(leftFirst: false))

        #expect(left.count == 1, "state must follow the keyed fragment")
        #expect(right.count == 0)
        #expect(host.log.allSatisfy { !$0.hasPrefix("create") })

        let labels = container.children[0].children.map { node -> String in
            guard case .string(let label)? = node.props["label"] else { return "?" }
            return label
        }
        #expect(labels == ["R", "L"])
    }

    @Test("an empty fragment contributes nothing and is not an error")
    func empty() {
        let (_, container, renderer) = makeRenderer()

        renderer.render(
            Node("app") {
                Node("a")
                Fragment {}
                Node("b")
            }
        )

        #expect(container.children[0].children.map(\.tag) == ["a", "b"])
    }
}
