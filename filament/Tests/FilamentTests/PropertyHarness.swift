import Foundation
import Testing

/// Case count and seed are overridable, so the same suite serves as a fast
/// check on every run and as a long hunt when you want one:
///
///     FILAMENT_PROPERTY_CASES=50000 FILAMENT_PROPERTY_SEED=7 swift test
///
/// The defaults are fixed rather than random. A suite that picks a new seed
/// every run is a suite that fails for one person and nobody else.
let propertyCaseCount = ProcessInfo.processInfo.environment["FILAMENT_PROPERTY_CASES"]
    .flatMap(Int.init) ?? 300

let propertySeed = ProcessInfo.processInfo.environment["FILAMENT_PROPERTY_SEED"]
    .flatMap(UInt64.init) ?? 0xF11A_3E27

/// A seeded, portable PRNG.
///
/// `SystemRandomNumberGenerator` cannot be seeded, which would make a failing
/// case unreproducible — the one thing a property test must never be. SplitMix64
/// is small, has no state to tune, and gives the same stream everywhere.
struct SplitMix64: RandomNumberGenerator {
    private var state: UInt64

    init(seed: UInt64) { state = seed }

    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
}

/// A property is a function from a scenario to a failure description, or `nil`
/// when the scenario holds.
///
/// It must be pure: the shrinker calls it many times on related inputs and
/// relies on the answer depending only on the scenario handed in.
typealias Property = @MainActor (Scenario) -> String?

/// Runs `property` against generated scenarios, shrinking the first failure to
/// something a human can read before reporting it.
@MainActor
func forAll(
    _ description: String,
    seed: UInt64 = propertySeed,
    cases: Int = propertyCaseCount,
    _ property: Property,
    sourceLocation: SourceLocation = #_sourceLocation
) {
    var seeds = SplitMix64(seed: seed)

    for index in 0..<cases {
        let caseSeed = seeds.next()
        var generator = SplitMix64(seed: caseSeed)
        let scenario = Scenario.random(using: &generator)

        guard let failure = property(scenario) else { continue }

        let (minimal, steps) = shrink(scenario, failing: property)
        let reason = property(minimal) ?? failure

        Issue.record(
            """
            Property failed: \(description)

            Case \(index) of \(cases), seed \(caseSeed).
            Shrunk over \(steps) reductions, from \(scenario.nodeCount) nodes \
            in \(scenario.steps.count) steps to \(minimal.nodeCount) nodes in \
            \(minimal.steps.count).

            \(reason)

            Minimal failing scenario:
            \(minimal.description)
            """,
            sourceLocation: sourceLocation
        )
        return
    }
}

/// Greedily reduces a failing scenario for as long as it keeps failing.
///
/// Each candidate is strictly smaller than its parent, so the loop terminates
/// on its own; the iteration cap only bounds how long a pathological case can
/// spend before reporting something useful.
@MainActor
private func shrink(_ scenario: Scenario, failing property: Property) -> (Scenario, Int) {
    var current = scenario
    var reductions = 0
    let maxReductions = 2_000

    var progressed = true
    while progressed, reductions < maxReductions {
        progressed = false
        for candidate in current.shrinkCandidates() {
            reductions += 1
            if reductions >= maxReductions { break }
            if property(candidate) != nil {
                current = candidate
                progressed = true
                break
            }
        }
    }

    return (current, reductions)
}
