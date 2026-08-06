import Filament

/// Holds the toggle callbacks each row publishes during render, so the demo can
/// drive component-local state from the outside the way a real event would.
@MainActor
final class Registry {
    var toggles: [String: () -> Void] = [:]
}

struct TodoRow: Component {
    let title: String
    let registry: Registry

    func render() -> Element {
        let (done, setDone) = useState(false)
        registry.toggles[title] = { setDone { !$0 } }

        return Node("row", [
            "title": .string(title),
            "done": .bool(done),
        ])
    }
}

struct TodoList: Component {
    let titles: [String]
    let registry: Registry

    func render() -> Element {
        Node("list", ["count": .number(Double(titles.count))]) {
            for title in titles {
                Keyed(title, TodoRow(title: title, registry: registry))
            }
        }
    }
}

@MainActor
@main
enum Demo {
    static func main() {
        let host = TestHost()
        let container = host.makeContainer()
        let renderer = Reconciler(host: host, container: container)
        let registry = Registry()

        func step(_ title: String, _ work: () -> Void) {
            host.clearLog()
            work()
            print("\n\u{1B}[1m\(title)\u{1B}[0m")
            print("  mutations:")
            if host.log.isEmpty {
                print("    (none)")
            } else {
                for entry in host.log { print("    \(entry)") }
            }
            print("  tree:")
            for line in container.describe().split(separator: "\n") {
                print("    \(line)")
            }
        }

        step("1. first render — everything is new") {
            renderer.render(
                TodoList(titles: ["write", "test", "ship"], registry: registry).asElement()
            )
        }

        step("2. toggle one row — local state, one prop touched") {
            registry.toggles["test"]?()
        }

        step("3. re-render the same list — nothing changed, so nothing happens") {
            renderer.render(
                TodoList(titles: ["write", "test", "ship"], registry: registry).asElement()
            )
        }

        step("4. insert at the head — the rows already in order are left alone") {
            renderer.render(
                TodoList(titles: ["plan", "write", "test", "ship"], registry: registry).asElement()
            )
        }

        step("5. reorder — nodes move, and \"test\" keeps done=true through it") {
            renderer.render(
                TodoList(titles: ["ship", "test", "plan", "write"], registry: registry).asElement()
            )
        }

        step("6. remove two rows") {
            renderer.render(
                TodoList(titles: ["ship", "test"], registry: registry).asElement()
            )
        }

        print("")
    }
}
