import Testing
@testable import Filament

@Suite("Recording host")
@MainActor
struct RecordingHostTests {
    private func makeRenderer() -> (RecordingHost<TestHost>, TestNode, Reconciler<RecordingHost<TestHost>>) {
        let base = TestHost()
        let container = base.makeContainer()
        let recorder = RecordingHost(base)
        recorder.name(container, "root")
        return (recorder, container, Reconciler(host: recorder, container: container))
    }

    private func row(_ names: [String], selected: String) -> Element {
        Node("row") {
            for name in names {
                Node("chip", key: name, [
                    "title": .string(name),
                    "selected": .bool(name == selected),
                ])
            }
        }
    }

    @Test("a first render is all creates")
    func mount() {
        let (recorder, _, renderer) = makeRenderer()
        renderer.render(row(["a", "b", "c"], selected: "a"))

        #expect(recorder.tally.created == 4, "three chips and the row")
        #expect(recorder.tally.updated == 0)
        #expect(recorder.tally.moved == 0)
        #expect(recorder.tally.removed == 0)
    }

    @Test("changing the selection is two updates and nothing else")
    func selectionChange() {
        let (recorder, _, renderer) = makeRenderer()
        renderer.render(row(["a", "b", "c"], selected: "a"))

        recorder.resetTally()
        renderer.render(row(["a", "b", "c"], selected: "c"))

        #expect(recorder.tally.created == 0, "this is the claim: nothing is rebuilt")
        #expect(recorder.tally.updated == 2, "the chip losing it and the chip gaining it")
        #expect(recorder.tally.inserted == 0)
        #expect(recorder.tally.moved == 0)
        #expect(recorder.tally.removed == 0)
    }

    @Test("an unchanged render costs nothing at all")
    func idempotent() {
        let (recorder, _, renderer) = makeRenderer()
        renderer.render(row(["a", "b"], selected: "a"))

        recorder.resetTally()
        renderer.render(row(["a", "b"], selected: "a"))

        #expect(recorder.tally.isEmpty)
    }

    @Test("a reorder is moves, never creates")
    func reorder() {
        let (recorder, _, renderer) = makeRenderer()
        renderer.render(row(["a", "b", "c"], selected: "a"))

        recorder.resetTally()
        renderer.render(row(["c", "a", "b"], selected: "a"))

        #expect(recorder.tally.created == 0)
        #expect(recorder.tally.moved > 0)
        #expect(recorder.tally.removed == 0)
    }

    @Test("events read as a legible trace")
    func eventText() {
        let (recorder, _, renderer) = makeRenderer()
        var lines: [String] = []
        recorder.onEvent = { lines.append($0.line) }

        renderer.render(row(["a"], selected: "a"))
        lines.removeAll()
        renderer.render(row(["a"], selected: "b"))

        #expect(lines == ["update chip#2 +selected"])
    }

    @Test("the wrapped host still receives everything")
    func passthrough() {
        let (recorder, container, renderer) = makeRenderer()
        renderer.render(row(["a", "b"], selected: "a"))
        recorder.resetTally()

        renderer.render(row(["b"], selected: "b"))

        // The point of a decorator is that it observes without interfering.
        #expect(container.children[0].children.count == 1)
        #expect(recorder.tally.removed == 1)
    }
}
