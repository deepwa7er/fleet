/// A single hook's persistent storage, living on the fiber.
@MainActor
public protocol HookSlot: AnyObject {}

final class StateSlot<Value>: HookSlot {
    var value: Value
    init(_ value: Value) { self.value = value }
}

final class EffectSlot: HookSlot {
    var dependencies: [AnyHashable]?
    var cleanup: (@MainActor () -> Void)?
    init(dependencies: [AnyHashable]?) { self.dependencies = dependencies }
}

/// The minimum a hook needs from the fiber it is running inside.
///
/// `Fiber` is generic over the host's instance type, but hooks are completely
/// indifferent to the host. This protocol erases that parameter so `useState`
/// does not have to be generic over a backend it never touches.
@MainActor
protocol HookHost: AnyObject {
    var hooks: [any HookSlot] { get set }
    var isMounted: Bool { get }
    var requestUpdate: (@MainActor () -> Void)? { get }
}

extension Fiber: HookHost {}

/// Effects deferred until after host mutations are applied.
@MainActor
final class EffectQueue {
    private var pending: [@MainActor () -> Void] = []
    func enqueue(_ work: @escaping @MainActor () -> Void) { pending.append(work) }
    func drain() -> [@MainActor () -> Void] {
        defer { pending = [] }
        return pending
    }
}

/// The ambient "which component am I rendering right now?" state.
///
/// Hooks are identified by call order, so they need exactly two things: the
/// fiber currently rendering, and how many hooks it has consumed so far. That
/// is the whole mechanism — the reason `useState` cannot appear inside an `if`
/// is that a skipped call shifts every subsequent index by one.
@MainActor
enum HookContext {
    static var current: (any HookHost)?
    static var cursor = 0
    static var effects: EffectQueue?

    static func begin(_ fiber: any HookHost, effects: EffectQueue) {
        current = fiber
        cursor = 0
        self.effects = effects
    }

    static func end() {
        current = nil
        cursor = 0
        effects = nil
    }

    /// Reserves the next slot index for the hook being called.
    static func claimSlot(_ hookName: String) -> (fiber: any HookHost, index: Int) {
        guard let fiber = current else {
            fatalError("\(hookName) was called outside of a component's render()")
        }
        let index = cursor
        cursor += 1
        return (fiber, index)
    }
}

// MARK: - useState

/// A state setter supporting both a direct value and an updater function.
///
/// The updater form exists because the direct form closes over the value read
/// during *this* render. Two `setCount(count + 1)` calls in one event both see
/// the same `count`; `setCount { $0 + 1 }` reads the live slot instead.
public struct Setter<Value> {
    let apply: @MainActor (@escaping (Value) -> Value) -> Void

    @MainActor
    public func callAsFunction(_ value: Value) { apply { _ in value } }

    @MainActor
    public func callAsFunction(_ transform: @escaping (Value) -> Value) { apply(transform) }
}

/// Declares a piece of state local to the calling component.
///
/// The returned value is a snapshot for this render. The setter writes into the
/// fiber's hook storage and asks the reconciler to re-render that fiber.
@MainActor
public func useState<Value>(_ initialValue: @autoclosure () -> Value) -> (Value, Setter<Value>) {
    let (fiber, index) = HookContext.claimSlot("useState")

    let slot: StateSlot<Value>
    if index == fiber.hooks.count {
        slot = StateSlot(initialValue())
        fiber.hooks.append(slot)
    } else {
        guard let existing = fiber.hooks[index] as? StateSlot<Value> else {
            fatalError(hookOrderViolation(index: index, expected: "useState"))
        }
        slot = existing
    }

    let setter = Setter<Value> { [weak fiber] transform in
        // A setter captured by a detached closure must not resurrect a fiber
        // that has already been unmounted.
        guard let fiber, fiber.isMounted else { return }
        slot.value = transform(slot.value)
        fiber.requestUpdate?()
    }

    return (slot.value, setter)
}

// MARK: - useEffect

/// Registers work to run after the host tree has been mutated.
///
/// Pass `nil` dependencies to run after every render, `[]` to run once on
/// mount, or a list to re-run whenever it changes. The returned closure, if
/// any, is the cleanup, run before the next invocation and on unmount.
@MainActor
public func useEffect(
    _ dependencies: [AnyHashable]?,
    _ effect: @escaping @MainActor () -> (@MainActor () -> Void)?
) {
    let (fiber, index) = HookContext.claimSlot("useEffect")
    guard let queue = HookContext.effects else {
        fatalError("useEffect was called outside of a render pass")
    }

    if index == fiber.hooks.count {
        let slot = EffectSlot(dependencies: dependencies)
        fiber.hooks.append(slot)
        queue.enqueue { slot.cleanup = effect() }
        return
    }

    guard let slot = fiber.hooks[index] as? EffectSlot else {
        fatalError(hookOrderViolation(index: index, expected: "useEffect"))
    }

    let shouldRun: Bool
    if let dependencies, let previous = slot.dependencies {
        shouldRun = dependencies != previous
    } else {
        shouldRun = true
    }
    slot.dependencies = dependencies

    if shouldRun {
        queue.enqueue {
            slot.cleanup?()
            slot.cleanup = effect()
        }
    }
}

/// Convenience for effects that need no cleanup.
@MainActor
public func useEffect(_ dependencies: [AnyHashable]?, _ effect: @escaping @MainActor () -> Void) {
    useEffect(dependencies) { effect(); return nil }
}

private func hookOrderViolation(index: Int, expected: String) -> String {
    """
    Hook order changed between renders: slot \(index) was not a \(expected).
    Hooks are matched by call order, so they must run unconditionally and in \
    the same sequence on every render — never inside an `if`, loop, or `guard`.
    """
}
