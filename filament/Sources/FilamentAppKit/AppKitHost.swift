#if canImport(AppKit)

import AppKit
import Filament

/// Implemented by views that accept prop changes after construction.
///
/// The reconciler hands over only what actually changed, so a view never has to
/// work out which of its attributes are stale — that question was already
/// answered upstream, without the view's help.
@MainActor
public protocol PropApplying: AnyObject {
    func applyProps(updated: [String: Props.Value], removed: [String])
}

/// A host backend that renders into real `NSView`s.
///
/// It deliberately does not know how to build any particular view. An app
/// registers a factory per tag, so its existing hand-written views — with their
/// own drawing, tracking areas and gestures — keep working exactly as they are.
/// What changes is who decides *which* views exist and when they update.
///
/// ## What this host does not do
///
/// Layout. `react-dom` sets attributes and lets CSS position things, and this
/// is the same: the host arranges views in the subview list and sets nothing
/// else. Frames remain the app's business, computed after the reconciler has
/// settled the tree.
///
/// ## The one invariant
///
/// Every subview of a container the reconciler manages must have been put there
/// by the reconciler. Views added behind its back shift the positions its
/// anchors are computed against, and the mismatch shows up later as views in
/// the wrong order rather than as an error at the point of the mistake. The
/// preconditions below catch the cases that are detectable locally; the rest is
/// a discipline the app has to keep.
@MainActor
public final class AppKitHost: HostConfig {
    public typealias Instance = NSView

    private var factories: [String: @MainActor (Props) -> NSView] = [:]
    private var textFactory: (@MainActor (String) -> NSView)?
    private var textUpdater: (@MainActor (NSView, String) -> Void)?

    public init() {}

    /// Registers the view type backing `tag`.
    ///
    /// The factory receives the element's initial props. Later changes arrive
    /// through `PropApplying`, so any view whose props can change must conform.
    public func register(_ tag: String, make: @escaping @MainActor (Props) -> NSView) {
        factories[tag] = make
    }

    /// Registers how bare strings in an element tree become views.
    ///
    /// Optional: a tree with no text nodes never needs it. Supplying it is all
    /// or nothing, because a text node with no way to build it has no sensible
    /// fallback.
    public func registerText(
        make: @escaping @MainActor (String) -> NSView,
        update: @escaping @MainActor (NSView, String) -> Void
    ) {
        textFactory = make
        textUpdater = update
    }

    // MARK: - HostConfig

    public func createInstance(tag: String, props: Props) -> NSView {
        guard let factory = factories[tag] else {
            preconditionFailure(
                """
                No view is registered for the tag "\(tag)". Call \
                AppKitHost.register("\(tag)") { props in ... } before rendering \
                a tree that uses it.
                """
            )
        }
        return factory(props)
    }

    public func createText(_ text: String) -> NSView {
        guard let textFactory else {
            preconditionFailure(
                """
                The element tree contains the text node "\(text)" but no text \
                factory is registered. Call AppKitHost.registerText(make:update:), \
                or wrap the string in a registered view.
                """
            )
        }
        return textFactory(text)
    }

    public func updateInstance(
        _ instance: NSView,
        updated: [String: Props.Value],
        removed: [String]
    ) {
        guard let target = instance as? PropApplying else {
            preconditionFailure(
                """
                \(type(of: instance)) received a prop change \
                (\(updated.keys.sorted().joined(separator: ", "))) but does not \
                conform to PropApplying, so the change would be dropped silently. \
                Either conform it, or stop changing its props after construction.
                """
            )
        }
        target.applyProps(updated: updated, removed: removed)
    }

    public func updateText(_ instance: NSView, text: String) {
        guard let textUpdater else {
            preconditionFailure("A text node changed but no text updater is registered.")
        }
        textUpdater(instance, text)
    }

    /// Places `child` in `parent`'s subview list immediately before `anchor`.
    ///
    /// `subviews` is ordered back to front, so "before" in the element tree is
    /// `.below` in AppKit's ordering. Re-inserting a view already in `parent`
    /// moves it, which is exactly what a reconciled reorder needs.
    public func insert(_ child: NSView, into parent: NSView, before anchor: NSView?) {
        precondition(
            child.superview == nil || child.superview === parent,
            "Cannot insert a view that is already a subview of a different parent."
        )

        guard let anchor else {
            parent.addSubview(child)
            return
        }

        precondition(
            anchor.superview === parent,
            """
            The anchor is not a subview of the parent, which means something \
            outside the reconciler changed this view tree.
            """
        )
        parent.addSubview(child, positioned: .below, relativeTo: anchor)
    }

    public func remove(_ child: NSView, from parent: NSView) {
        precondition(
            child.superview === parent,
            """
            Asked to detach a view from a parent it is not attached to, which \
            means something outside the reconciler changed this view tree.
            """
        )
        child.removeFromSuperview()
    }
}

#endif
