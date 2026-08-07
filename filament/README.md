# Filament

A minimal React, written from scratch in Swift.

It has components, fragments, a virtual tree, keyed reconciliation, hooks,
effect cleanup and update batching — about 1,000 lines of commented Swift, no
dependencies.

The reconciler renders through a host protocol, so it ships with two backends:
an in-memory one that lets you watch the diffing algorithm work with no DOM, no
browser and no build step, and an AppKit one that drives real `NSView`s.

```
swift test               # 52 tests, including a property suite
swift run filament-demo
```

## What it looks like

```swift
struct TodoRow: Component {
    let title: String

    func render() -> Element {
        let (done, setDone) = useState(false)

        return Node("row", [
            "title": .string(title),
            "done": .bool(done),
            "onTap": .handler { setDone { !$0 } },
        ])
    }
}

struct TodoList: Component {
    let titles: [String]

    func render() -> Element {
        Node("list", ["count": .number(Double(titles.count))]) {
            for title in titles {
                Keyed(title, TodoRow(title: title))
            }
        }
    }
}
```

That nesting syntax is a [result builder][rb]. JSX needs a compiler plugin to
get the same shape; Swift ships it as a language feature, so `if` and `for`
inside a tree work without anyone special-casing them.

[rb]: https://docs.swift.org/swift-book/documentation/the-swift-programming-language/advancedoperators/#Result-Builders

## The one idea

Elements are values, describing what the UI should be. They are cheap, they are
thrown away and rebuilt on every render, and creating one touches nothing real.

Fibers are the persistent tree that mirrors them. A fiber survives across
renders, which is the entire trick behind hooks: `useState` needs somewhere to
put a value that outlives the function call that created it, and the fiber is
that somewhere.

The reconciler's whole job is to walk a new element tree against the existing
fiber tree and emit the smallest set of host mutations that reconciles the two.

```
Element tree  ──────►  Reconciler  ──────►  HostConfig
(cheap, thrown away)   (persistent          (TestNode here, a DOM node
                        fiber tree)          or an NSView elsewhere)
```

## Reading it in order

| File | What it holds |
| --- | --- |
| `Element.swift` | The value tree, and the type identity that decides update-vs-rebuild |
| `Props.swift` | Attributes, and the prop diff |
| `ElementBuilder.swift` | The result builder — the JSX layer |
| `Fiber.swift` | The persistent node |
| `Hooks.swift` | `useState`, `useEffect`, and the call-order machinery |
| `HostConfig.swift` | The backend protocol |
| `Reconciler.swift` | The algorithm |
| `TestHost.swift` | An in-memory backend that logs every mutation |
| `FilamentAppKit/AppKitHost.swift` | A backend driving real `NSView`s |

Start with `Reconciler.reconcileChildren`. Everything else exists to serve it.

## Three things worth understanding

**Hooks are an array and a cursor.** `HookContext` holds the fiber currently
rendering and how many hooks it has consumed. `useState` takes the next slot.
That is the whole mechanism — and it is also the whole reason a hook cannot live
inside an `if`, because a skipped call shifts every later index by one. Filament
traps loudly when that happens rather than silently handing you another
component's state.

**Keys are identity.** Without a key, a child's only identity is its position
among its siblings, so reordering a list hands each slot's state to whatever
lands there. `KeyTests` asserts both halves of this: keyed children carry their
state through a reorder, and unkeyed ones demonstrably do not. Taking "identity"
literally is also why duplicate sibling keys are a hard error here — two
children cannot both be the same child.

**Render and commit are separate phases.** The render phase decides what is new,
updated, moved or gone and flags fibers accordingly; it never inserts anything,
because an insertion needs to know where every sibling ends up and that is not
known until the child list is fully reconciled. The commit phase then walks the
finished tree once and applies deletions and placements in order. Effects run
last, against a host tree that is already consistent.

## Why the host is a protocol

Nothing above `HostConfig` knows what a DOM node or a terminal cell is. That is
the same split React makes between `react-reconciler` and `react-dom`, and it is
what lets the interesting code be tested against plain objects.

`TestHost` records every mutation it is asked to perform, which is what makes
the central claim testable. Asserting on the final tree only proves the output
is right; asserting on the log proves the reconciler *diffed* rather than quietly
rebuilding the world, which is the entire promise a virtual DOM makes:

```
2. toggle one row — local state, one prop touched
  mutations:
    update row#3 +done

4. insert at the head — the rows already in order are left alone
  mutations:
    update list#1 +count
    create row#5
    insert row#5 into list#1 before row#2
```

Writing a real backend means implementing six methods. `FilamentAppKit` is that
proof: it renders into real `NSView`s, knows how to build none of them itself
(an app registers a factory per tag, so its existing hand-written views keep
their drawing and gestures), and does no layout — because `react-dom` does no
layout either. Frames stay the app's business, computed once the reconciler has
settled the tree.

## Proving it

Example-based tests only prove the cases someone thought of. The property suite
generates random trees, mutates them into random sequences of related trees, and
asserts invariants over whatever comes out:

| Property | Claim |
| --- | --- |
| **convergence** | However it got there, the incrementally diffed tree equals a fresh render of the same description |
| **idempotence** | Re-rendering an unchanged description performs no host work at all |
| **structural integrity** | No node is ever reachable twice — every insert has its matching detach |
| **effect balance** | Every effect that ran is cleaned up exactly once, across reorders and type swaps |
| **no premature cleanup** | ...and never cleaned up while still mounted |
| **keyed list minimality** | Any permutation preserves state and creates nothing; one insertion is one placement; one deletion is one removal |

Steps are *related* rather than independently random. A fresh tree each step
would remount everything and never exercise a move, an in-place update, or state
surviving a reorder, which is most of what there is to get wrong.

Failures shrink. The duplicate-key trap below was found by `idempotence` and
reported as a 3-node, single-step scenario reduced from a 22-node, 4-step one —
the difference between a bug you can read and a bug you have to excavate.

Everything is seeded and reproducible, and the budget is adjustable:

```
FILAMENT_PROPERTY_CASES=25000 FILAMENT_PROPERTY_SEED=7 swift test
```

`GeneratorCoverageTests` is what keeps the above honest. A property suite that
passes because its generator only ever produced two-node unkeyed trees proves
nothing, and a green run would not tell you. So the generator's output is
measured and the suite fails when any interesting case dries up:

```
Generator coverage over 300 cases:
  node moved                         74 (24.7%)
  node removed                       233 (77.7%)
  mixed keyed/unkeyed siblings       263 (87.7%)
  component inside component         36 (12.0%)
  ...
```

That check has already paid for itself once: moves initially appeared in 3.7% of
cases, meaning the hardest part of the reconciler was barely being tested.

## What this deliberately is not

Honest list of the simplifications, so nobody mistakes this for a React
replacement:

- **Rendering is synchronous and cannot be interrupted.** React links fibers as
  child/sibling/return precisely so a render can be paused and resumed; Filament
  uses a plain child array and ordinary recursion. No time slicing, no
  priorities, no Suspense, no concurrent features.
- **Prop and text updates are applied during the render phase**, not deferred to
  commit. That is sound here only because a render always runs to completion. It
  would be a correctness bug under interruptible rendering.
- **Only `useState` and `useEffect`.** No context, refs, reducers, memo, or
  layout effects. `memo` in particular would be a props-equality bailout in
  exactly one place, marked in `Reconciler.update`.
- **Handlers always count as changed**, because two Swift closures are never
  comparable. The practical consequence is that any node carrying a handler
  reports a prop update on every render — see `handlersAlwaysCountAsChanged`.
  React does not solve this either (it is not solvable); it sidesteps it with
  root-level event delegation, so the handler is looked up at dispatch time
  instead of being written to the node. A real host backend here would want the
  same trick.
- **No error boundaries.** Caller mistakes trap rather than degrading: a
  hook-order violation, and two siblings sharing a key. React warns on duplicate
  keys and proceeds, which is a backwards-compatibility compromise this codebase
  does not need — two children claiming one identity has no correct resolution,
  so one of them silently loses its state.
- **Single-threaded**, `@MainActor` throughout.

## Prior art

React's own [`react-reconciler`][rr] is the reference for the host-config split
and the `lastPlacedIndex` move heuristic. [Build your own React][didact] by
Rodrigo Pombo is the best short walkthrough of the same ideas in JavaScript.

[rr]: https://github.com/facebook/react/tree/main/packages/react-reconciler
[didact]: https://pomb.us/build-your-own-react/
