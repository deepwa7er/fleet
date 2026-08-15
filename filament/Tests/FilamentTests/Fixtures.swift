import Filament

/// A handle a test keeps on a component instance, so it can drive state from
/// outside and count how often the component actually re-rendered.
@MainActor
final class Probe {
    var renders = 0
    var setCount: Setter<Int>?
    var count = 0

    func bump() {
        guard let setCount else { return }
        setCount { $0 + 1 }
    }
}

/// A component with local state. `label` is a prop, `count` is state — the
/// distinction the whole exercise turns on.
struct Counter: Component {
    let probe: Probe
    let label: String

    func render() -> Element {
        let (count, setCount) = useState(0)
        probe.renders += 1
        probe.setCount = setCount
        probe.count = count
        return Node("counter", [
            "label": .string(label),
            "count": .number(Double(count)),
        ])
    }
}

/// A component that renders a different *type* depending on a flag, used to
/// prove that a type change tears the subtree down rather than updating it.
struct Swapper: Component {
    let showCounter: Bool
    let probe: Probe

    func render() -> Element {
        Node("swapper") {
            if showCounter {
                Counter(probe: probe, label: "inner")
            } else {
                Node("placeholder")
            }
        }
    }
}

/// A list whose children are optionally keyed, so tests can compare the two.
struct List: Component {
    let names: [String]
    let probes: [String: Probe]
    let keyed: Bool

    func render() -> Element {
        Node("list") {
            for name in names {
                if keyed {
                    Keyed(name, Counter(probe: probes[name]!, label: name))
                } else {
                    Counter(probe: probes[name]!, label: name).asElement()
                }
            }
        }
    }
}

/// Records effect lifecycle in call order.
@MainActor
final class EffectProbe {
    var events: [String] = []
}

struct Effectful: Component {
    let probe: EffectProbe
    let value: Int
    let watchValue: Bool

    func render() -> Element {
        useEffect(watchValue ? [AnyHashable(value)] : []) {
            probe.events.append("run \(value)")
            return { probe.events.append("cleanup \(value)") }
        }
        return Node("box", ["v": .number(Double(value))])
    }
}

/// Holds two independent pieces of state, for testing update batching.
@MainActor
final class PairProbe {
    var renders = 0
    var setA: Setter<Int>?
    var setB: Setter<Int>?
    var a = 0
    var b = 0
}

struct Pair: Component {
    let probe: PairProbe

    func render() -> Element {
        let (a, setA) = useState(0)
        let (b, setB) = useState(0)
        probe.renders += 1
        probe.setA = setA
        probe.setB = setB
        probe.a = a
        probe.b = b
        return Node("pair", [
            "a": .number(Double(a)),
            "b": .number(Double(b)),
        ])
    }
}

/// A component whose output is a component, so a placement has to move host
/// nodes it does not own.
struct Wrapper: Component {
    let probe: Probe
    let label: String

    func render() -> Element {
        Counter(probe: probe, label: label).asElement()
    }
}

struct WrapperList: Component {
    let names: [String]
    let probes: [String: Probe]

    func render() -> Element {
        Node("list") {
            for name in names {
                Keyed(name, Wrapper(probe: probes[name]!, label: name))
            }
        }
    }
}
