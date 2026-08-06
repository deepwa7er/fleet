# Filament

A minimal React, written from scratch in Swift.

It has components, a virtual tree, keyed reconciliation, hooks, effect cleanup
and update batching — about 900 lines of commented Swift, no dependencies. It
renders into an
in-memory host, so you can watch the diffing algorithm work without a DOM, a
browser, or a build step anywhere in the picture.

```
swift test          # 20 tests
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
state through a reorder, and unkeyed ones demonstrably do not.

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

Writing a real backend means implementing six methods.

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
- **No error boundaries.** A hook-order violation traps.
- **Single-threaded**, `@MainActor` throughout.

## Prior art

React's own [`react-reconciler`][rr] is the reference for the host-config split
and the `lastPlacedIndex` move heuristic. [Build your own React][didact] by
Rodrigo Pombo is the best short walkthrough of the same ideas in JavaScript.

[rr]: https://github.com/facebook/react/tree/main/packages/react-reconciler
[didact]: https://pomb.us/build-your-own-react/
