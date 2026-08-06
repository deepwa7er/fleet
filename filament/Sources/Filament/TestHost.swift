/// A node in the in-memory tree the `TestHost` maintains.
public final class TestNode {
    public let tag: String
    public private(set) var props: Props
    public private(set) var text: String?
    public private(set) var children: [TestNode] = []

    /// Distinguishes "this node was updated" from "this node was thrown away
    /// and an identical one built", which is the single most important thing a
    /// reconciler test needs to be able to tell apart.
    public let serial: Int

    init(tag: String, props: Props, serial: Int) {
        self.tag = tag
        self.props = props
        self.text = nil
        self.serial = serial
    }

    init(text: String, serial: Int) {
        self.tag = "#text"
        self.props = Props()
        self.text = text
        self.serial = serial
    }

    func apply(updated: [String: Props.Value], removed: [String]) {
        var storage = props.storage
        for (key, value) in updated { storage[key] = value }
        for key in removed { storage.removeValue(forKey: key) }
        props = Props(storage)
    }

    func setText(_ value: String) { text = value }

    func insert(_ child: TestNode, before anchor: TestNode?) {
        // A re-insert of an existing child is a move, so detach first.
        children.removeAll { $0 === child }
        if let anchor, let index = children.firstIndex(where: { $0 === anchor }) {
            children.insert(child, at: index)
        } else {
            children.append(child)
        }
    }

    func remove(_ child: TestNode) {
        children.removeAll { $0 === child }
    }

    /// An indented rendering of the subtree, for snapshot assertions.
    public func describe(indent: Int = 0) -> String {
        let pad = String(repeating: "  ", count: indent)
        var line: String
        if let text {
            line = "\(pad)\"\(text)\""
        } else {
            let attributes = props.storage
                .sorted { $0.key < $1.key }
                .map { "\($0.key)=\($0.value.description)" }
                .joined(separator: " ")
            line = attributes.isEmpty ? "\(pad)<\(tag)>" : "\(pad)<\(tag) \(attributes)>"
        }
        for child in children {
            line += "\n" + child.describe(indent: indent + 1)
        }
        return line
    }
}

/// A host backend that renders into plain objects and records every mutation.
///
/// The mutation log is the point. Asserting on the final tree only proves the
/// output is right; asserting on the log proves the reconciler *diffed* rather
/// than quietly rebuilding the world each render, which is the entire claim a
/// virtual DOM makes.
public final class TestHost: HostConfig {
    public private(set) var log: [String] = []
    private var nextSerial = 0

    public init() {}

    /// The root the reconciler renders into. Serial 0 marks it as pre-existing
    /// rather than something the reconciler created.
    public func makeContainer() -> TestNode {
        TestNode(tag: "#root", props: Props(), serial: 0)
    }

    public func clearLog() { log.removeAll() }

    private func serial() -> Int {
        nextSerial += 1
        return nextSerial
    }

    private func label(_ node: TestNode) -> String {
        if let text = node.text { return "\"\(text)\"#\(node.serial)" }
        return "\(node.tag)#\(node.serial)"
    }

    // MARK: HostConfig

    public func createInstance(tag: String, props: Props) -> TestNode {
        let node = TestNode(tag: tag, props: props, serial: serial())
        log.append("create \(label(node))")
        return node
    }

    public func createText(_ text: String) -> TestNode {
        let node = TestNode(text: text, serial: serial())
        log.append("create \(label(node))")
        return node
    }

    public func updateInstance(
        _ instance: TestNode,
        updated: [String: Props.Value],
        removed: [String]
    ) {
        instance.apply(updated: updated, removed: removed)
        let changes = updated.keys.sorted().map { "+\($0)" } + removed.sorted().map { "-\($0)" }
        log.append("update \(label(instance)) \(changes.joined(separator: ","))")
    }

    public func updateText(_ instance: TestNode, text: String) {
        instance.setText(text)
        log.append("text \(instance.serial) -> \"\(text)\"")
    }

    public func insert(_ child: TestNode, into parent: TestNode, before anchor: TestNode?) {
        let existing = parent.children.contains { $0 === child }
        parent.insert(child, before: anchor)
        let verb = existing ? "move" : "insert"
        let position = anchor.map { " before \(label($0))" } ?? ""
        log.append("\(verb) \(label(child)) into \(label(parent))\(position)")
    }

    public func remove(_ child: TestNode, from parent: TestNode) {
        parent.remove(child)
        log.append("remove \(label(child)) from \(label(parent))")
    }
}
