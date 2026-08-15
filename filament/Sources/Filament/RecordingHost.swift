/// Wraps another host and reports every mutation passing through it.
///
/// `TestHost` logs its own mutations, which is what makes the in-memory tests
/// convincing — they assert the reconciler *diffed* rather than rebuilt. A real
/// backend has no such window, so the one place the algorithm actually matters
/// is the one place you cannot watch it work.
///
/// This closes that gap for any backend, because logging is not a particular
/// host's business. Wrap a host, render through the wrapper, and the events are
/// the same evidence the tests rely on.
///
/// It is a debugging tool and is not free: to keep labels honest it holds a
/// strong reference to every instance it has named. Object addresses are
/// reused after deallocation, so without that a freed view's identity could be
/// handed to a new one and the log would confidently attribute events to the
/// wrong node. Correct labels are worth more than the memory here, but this is
/// not something to leave wrapped in a shipping build.
@MainActor
public final class RecordingHost<Base: HostConfig>: HostConfig {
    public typealias Instance = Base.Instance

    public struct Event {
        public enum Kind: String {
            case create, update, text, insert, move, remove
        }

        public let kind: Kind
        public let subject: String
        public let detail: String

        public var line: String {
            detail.isEmpty ? "\(kind.rawValue) \(subject)" : "\(kind.rawValue) \(subject) \(detail)"
        }
    }

    /// What one render pass cost, which is the number that settles the argument.
    public struct Tally {
        public var created = 0
        public var updated = 0
        public var inserted = 0
        public var moved = 0
        public var removed = 0

        public var isEmpty: Bool {
            created == 0 && updated == 0 && inserted == 0 && moved == 0 && removed == 0
        }

        public var summary: String {
            "\(created) created · \(updated) updated · \(inserted) inserted · "
                + "\(moved) moved · \(removed) removed"
        }
    }

    private let base: Base

    /// Instances are retained deliberately — see the note on the type.
    private var known: [ObjectIdentifier: (instance: Instance, label: String)] = [:]
    private var parents: [ObjectIdentifier: ObjectIdentifier] = [:]
    private var nextSerial = 0

    public private(set) var tally = Tally()
    public var onEvent: (@MainActor (Event) -> Void)?

    public init(_ base: Base) {
        self.base = base
    }

    /// Gives an instance the recorder did not create a readable name, so log
    /// lines say "into chips" rather than "into view#0".
    public func name(_ instance: Instance, _ label: String) {
        known[ObjectIdentifier(instance)] = (instance, label)
    }

    /// Starts a fresh count. Call before a render to measure just that pass.
    public func resetTally() {
        tally = Tally()
    }

    // MARK: - HostConfig

    public func createInstance(tag: String, props: Props) -> Instance {
        let instance = base.createInstance(tag: tag, props: props)
        let label = register(instance, tag: tag)
        tally.created += 1
        emit(.create, label, "")
        return instance
    }

    public func createText(_ text: String) -> Instance {
        let instance = base.createText(text)
        let label = register(instance, tag: "text")
        tally.created += 1
        emit(.create, label, "\"\(text)\"")
        return instance
    }

    public func updateInstance(
        _ instance: Instance,
        updated: [String: Props.Value],
        removed: [String]
    ) {
        base.updateInstance(instance, updated: updated, removed: removed)
        tally.updated += 1
        let changes = updated.keys.sorted().map { "+\($0)" } + removed.sorted().map { "-\($0)" }
        emit(.update, label(of: instance), changes.joined(separator: ","))
    }

    public func updateText(_ instance: Instance, text: String) {
        base.updateText(instance, text: text)
        tally.updated += 1
        emit(.text, label(of: instance), "\"\(text)\"")
    }

    public func insert(_ child: Instance, into parent: Instance, before anchor: Instance?) {
        // A child already under this parent is being repositioned, not added.
        // The distinction is the whole point of the log: an insert is work the
        // tree genuinely needed, a move is work the ordering needed, and a
        // create where neither was expected is the reconciler failing.
        let isMove = parents[ObjectIdentifier(child)] == ObjectIdentifier(parent)

        base.insert(child, into: parent, before: anchor)
        parents[ObjectIdentifier(child)] = ObjectIdentifier(parent)

        let position = anchor.map { " before \(label(of: $0))" } ?? ""
        let detail = "into \(label(of: parent))\(position)"

        if isMove {
            tally.moved += 1
            emit(.move, label(of: child), detail)
        } else {
            tally.inserted += 1
            emit(.insert, label(of: child), detail)
        }
    }

    public func remove(_ child: Instance, from parent: Instance) {
        base.remove(child, from: parent)
        parents.removeValue(forKey: ObjectIdentifier(child))
        tally.removed += 1
        emit(.remove, label(of: child), "from \(label(of: parent))")
    }

    // MARK: - Labelling

    private func register(_ instance: Instance, tag: String) -> String {
        nextSerial += 1
        let label = "\(tag)#\(nextSerial)"
        known[ObjectIdentifier(instance)] = (instance, label)
        return label
    }

    private func label(of instance: Instance) -> String {
        if let existing = known[ObjectIdentifier(instance)] { return existing.label }
        return register(instance, tag: "view")
    }

    private func emit(_ kind: Event.Kind, _ subject: String, _ detail: String) {
        onEvent?(Event(kind: kind, subject: subject, detail: detail))
    }
}
