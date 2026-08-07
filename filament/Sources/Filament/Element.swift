/// The immutable description of a UI tree.
///
/// An `Element` is a *value*: creating one allocates nothing on the host and
/// performs no work. It is a plan that the reconciler compares against the
/// previous plan to decide which host mutations are actually required.
public enum Element {
    case text(String)
    case host(HostElement)
    case component(ComponentElement)
    case fragment(FragmentElement)

    /// The reconciliation key, if the author supplied one.
    ///
    /// Keys give a child a stable identity that survives reordering. Without
    /// one, a child is identified purely by its position among its siblings.
    public var key: AnyHashable? {
        switch self {
        case .text: nil
        case .host(let h): h.key
        case .component(let c): c.key
        case .fragment(let f): f.key
        }
    }

    /// Identity used to decide "is this the same *kind* of thing as before?".
    ///
    /// When the type identity of an old and new element at the same position
    /// differ, the old subtree is torn down rather than updated — this is what
    /// makes swapping `Counter` for `Clock` reset state instead of leaking it.
    var typeIdentity: TypeIdentity {
        switch self {
        case .text: .text
        case .host(let h): .host(h.tag)
        case .component(let c): .component(c.typeID)
        case .fragment: .fragment
        }
    }
}

enum TypeIdentity: Equatable {
    case text
    case host(String)
    case component(ObjectIdentifier)
    case fragment
}

/// Several elements standing where one is expected.
///
/// A component returns exactly one element, and a host element is a real node
/// in the output — so without this there is no way to contribute a *list* of
/// children to somebody else's container without inventing a wrapper node that
/// exists only to satisfy the type system. A fragment is that missing shape: it
/// occupies a position in the element tree and creates nothing in the host.
public struct FragmentElement {
    public let children: [Element]
    public let key: AnyHashable?

    public init(children: [Element], key: AnyHashable? = nil) {
        self.children = children
        self.key = key
    }
}

// MARK: - Host elements

/// A leaf the host backend knows how to instantiate — the equivalent of
/// `"div"` or `"span"` in React DOM.
public struct HostElement {
    public let tag: String
    public let props: Props
    public let children: [Element]
    public let key: AnyHashable?

    public init(tag: String, props: Props, children: [Element], key: AnyHashable?) {
        self.tag = tag
        self.props = props
        self.children = children
        self.key = key
    }
}

// MARK: - Component elements

/// A user-defined component together with the props it was constructed with.
///
/// The component is stored as an existential rather than a closure so that its
/// *type* survives into the fiber tree. Swift closures have no usable identity,
/// so a closure-based design could not answer "same component as last time?".
public struct ComponentElement {
    public let component: any Component
    public let key: AnyHashable?

    var typeID: ObjectIdentifier { ObjectIdentifier(type(of: component)) }
}

/// A component is a value holding props with a single method producing UI.
///
/// Conforming types should be structs: the reconciler discards and rebuilds the
/// value on every render, and all state lives in hooks rather than in the value.
@MainActor
public protocol Component {
    func render() -> Element
}

extension Component {
    /// Lifts a component value into an `Element` so it can appear in a tree.
    public func asElement(key: AnyHashable? = nil) -> Element {
        .component(ComponentElement(component: self, key: key))
    }
}

// MARK: - Construction helpers

/// Creates a host element.
///
/// The trailing closure is a result builder, so children nest declaratively
/// with no code generation or source transform — the syntax layer JSX needs a
/// compiler plugin for is a plain language feature here.
@MainActor
public func Node(
    _ tag: String,
    key: AnyHashable? = nil,
    _ props: Props = [:],
    @ElementBuilder children: () -> [Element] = { [] }
) -> Element {
    .host(HostElement(tag: tag, props: props, children: children(), key: key))
}

/// Wraps a component in an element with an explicit reconciliation key.
@MainActor
public func Keyed(_ key: AnyHashable, _ component: some Component) -> Element {
    component.asElement(key: key)
}

/// Groups several elements into one without creating anything in the host.
///
/// Use it when a component's output is a list rather than a single node, so its
/// children land directly in whatever container the component was placed in.
@MainActor
public func Fragment(
    key: AnyHashable? = nil,
    @ElementBuilder _ children: () -> [Element]
) -> Element {
    .fragment(FragmentElement(children: children(), key: key))
}
