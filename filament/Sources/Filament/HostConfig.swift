/// The backend a reconciler renders into.
///
/// Nothing above this protocol knows what a DOM node, a terminal cell or an
/// `NSView` is. That separation is not decoration — it is the same split React
/// makes between `react-reconciler` and `react-dom`, and it is what lets the
/// diffing algorithm be tested against an in-memory tree with no platform in
/// the picture.
///
/// A host is asked only to create, mutate and arrange instances. It is never
/// asked to diff, because it is never told what changed beyond the minimal set
/// the reconciler already computed.
@MainActor
public protocol HostConfig: AnyObject {
    /// The backend's node type — `TestNode` here, `HTMLElement` in a browser.
    associatedtype Instance: AnyObject

    func createInstance(tag: String, props: Props) -> Instance
    func createText(_ text: String) -> Instance

    func updateInstance(_ instance: Instance, updated: [String: Props.Value], removed: [String])
    func updateText(_ instance: Instance, text: String)

    /// Inserts `child` into `parent` immediately before `anchor`, or at the end
    /// when `anchor` is `nil`.
    func insert(_ child: Instance, into parent: Instance, before anchor: Instance?)
    func remove(_ child: Instance, from parent: Instance)
}
