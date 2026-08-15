import Foundation
import Testing
@testable import Filament

/// Guards the property suite against becoming vacuous.
///
/// Properties only mean something if the generator actually reaches the
/// situations they describe. A suite that passes 700,000 cases because every
/// case was a two-node tree with no keys proves nothing, and nothing about a
/// green run would tell you that had happened. This test measures what the
/// generator produces and fails when any interesting case dries up.
@Suite("Generator coverage")
@MainActor
struct GeneratorCoverageTests {
    /// Each label must appear in at least this share of generated cases.
    /// Deliberately low: the point is to catch a category vanishing, not to
    /// pin down a distribution that will drift with any generator change.
    private static let defaultFloor = 0.02

    /// Moves are where the reconciler is hardest and most likely to be wrong,
    /// so thin coverage there is worth failing over well before it hits zero.
    private static let floors: [String: Double] = ["node moved": 0.15]

    private static func floor(for label: String) -> Double {
        floors[label] ?? defaultFloor
    }

    @Test("the generator reaches every situation the properties are about")
    func coverage() {
        var seeds = SplitMix64(seed: propertySeed &* 11)
        var casesWith: [String: Int] = [:]
        let total = propertyCaseCount

        for _ in 0..<total {
            var generator = SplitMix64(seed: seeds.next())
            let scenario = Scenario.random(using: &generator)
            var observed: Set<String> = []

            let world = World()
            for (index, shape) in scenario.steps.enumerated() {
                world.clearLog()
                world.render(shape)

                // Only mutations after the initial mount are interesting; the
                // first render trivially creates and inserts everything.
                guard index > 0 else { continue }
                for entry in world.log {
                    switch entry.prefix(while: { $0 != " " }) {
                    case "move": observed.insert("node moved")
                    case "remove": observed.insert("node removed")
                    case "create": observed.insert("node created after mount")
                    case "insert": observed.insert("node inserted after mount")
                    case "update": observed.insert("props updated in place")
                    case "text": observed.insert("text updated in place")
                    default: break
                    }
                }
            }

            for shape in scenario.steps {
                if shape.containsComponent { observed.insert("component in tree") }
                if shape.containsNestedComponent { observed.insert("component inside component") }
                if shape.hasMixedKeyedSiblings { observed.insert("mixed keyed/unkeyed siblings") }
                if shape.hasKeyedSiblingList { observed.insert("two or more keyed siblings") }
                if shape.hasKeyedComponentChild { observed.insert("keyed component child") }
                if shape.containsFragment { observed.insert("fragment in tree") }
                if shape.containsMultiChildFragment {
                    observed.insert("fragment with several children")
                }
            }

            if scenario.steps.count > 2 { observed.insert("three or more steps") }

            for label in observed { casesWith[label, default: 0] += 1 }
        }

        let expected = [
            "node moved",
            "node removed",
            "node created after mount",
            "node inserted after mount",
            "props updated in place",
            "text updated in place",
            "component in tree",
            "component inside component",
            "mixed keyed/unkeyed siblings",
            "two or more keyed siblings",
            "keyed component child",
            "fragment in tree",
            "fragment with several children",
            "three or more steps",
        ]

        let report = expected
            .map { label in
                let count = casesWith[label, default: 0]
                let share = Double(count) / Double(total) * 100
                let name = label.padding(toLength: 34, withPad: " ", startingAt: 0)
                return "  \(name) \(count) (\(String(format: "%.1f", share))%)"
            }
            .joined(separator: "\n")
        print("Generator coverage over \(total) cases:\n\(report)")

        for label in expected {
            let count = casesWith[label, default: 0]
            let share = Double(count) / Double(total)
            let floor = Self.floor(for: label)
            #expect(
                share >= floor,
                """
                The generator produced "\(label)" in only \(count) of \(total) cases \
                (\(String(format: "%.2f", share * 100))%), below the \
                \(String(format: "%.0f", floor * 100))% floor. The properties are no \
                longer exercising this case.
                """
            )
        }
    }
}
