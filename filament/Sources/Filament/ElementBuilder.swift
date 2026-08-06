/// Result builder that turns a nested block into a child list.
///
/// This is the piece JSX exists to provide. Swift ships it as a language
/// feature, so there is no transform step, no `createElement` calls in the
/// source, and `if` / `for` inside a tree work because the builder has
/// `buildOptional`, `buildEither` and `buildArray` rather than because anyone
/// special-cased them.
@resultBuilder
@MainActor
public enum ElementBuilder {
    public static func buildExpression(_ element: Element) -> [Element] { [element] }

    public static func buildExpression(_ component: some Component) -> [Element] {
        [component.asElement()]
    }

    /// A bare string in a tree becomes a text node.
    public static func buildExpression(_ text: String) -> [Element] { [.text(text)] }

    public static func buildExpression(_ elements: [Element]) -> [Element] { elements }

    public static func buildBlock(_ parts: [Element]...) -> [Element] { parts.flatMap(\.self) }

    public static func buildOptional(_ part: [Element]?) -> [Element] { part ?? [] }

    public static func buildEither(first part: [Element]) -> [Element] { part }

    public static func buildEither(second part: [Element]) -> [Element] { part }

    public static func buildArray(_ parts: [[Element]]) -> [Element] { parts.flatMap(\.self) }

    public static func buildLimitedAvailability(_ part: [Element]) -> [Element] { part }
}
