import AppKit
import Filament
import FilamentAppKit

/// Opt-in switches for work that is being migrated rather than replaced.
enum FeatureFlags {
    /// Drive the command panel's chip rows through Filament instead of the
    /// hand-rolled tear-down-and-rebuild path.
    ///
    ///     defaults write net.deepwa7er.loom FilamentChips -bool YES
    ///
    /// Off by default. Both paths render the same chips into the same
    /// containers, so the only difference is who decides which chips exist and
    /// when they change — which is exactly what needs comparing before the old
    /// path can go.
    static var filamentChips: Bool {
        UserDefaults.standard.bool(forKey: "FilamentChips")
    }
}

/// One chip's worth of state, independent of how it gets on screen.
///
/// Both the legacy and the Filament path build this list, so the two are
/// comparable: any difference on screen is a difference in rendering, not in
/// what was asked for.
struct ChipSpec {
    /// Stable identity, deliberately not the title.
    ///
    /// Two identical monitors report the same `localizedName`, so a title is
    /// not unique and two chips claiming one identity is a hard error. A title
    /// is also not *stable* — the login chip's label is its state — and a chip
    /// keyed on its label would be discarded and rebuilt every time it changed,
    /// which is the behaviour this is replacing.
    let id: String
    let title: String
    let isSelected: Bool
    let onClick: () -> Void
}

/// The tag the chip row's views are registered under.
private let chipTag = "chip"

/// Renders a row of chips as siblings, with no wrapper view of its own.
///
/// The fragment matters here: the chips must be direct subviews of the
/// container so the panel's existing layout can walk `container.subviews` and
/// position them. A component that returned a single node would put a view in
/// between and force the layout to reach through it.
struct ChipRow: Component {
    let specs: [ChipSpec]

    func render() -> Element {
        Fragment {
            for spec in specs {
                Node(chipTag, key: spec.id, [
                    "title": .string(spec.title),
                    "selected": .bool(spec.isSelected),
                    "onClick": .handler { spec.onClick() },
                ])
            }
        }
    }
}

/// Owns the reconciler for one chip container.
///
/// Kept deliberately small: the panel hands it a list of chips and it makes the
/// container's subviews match. It knows nothing about layout, which stays with
/// the panel.
@MainActor
final class ChipRowRenderer {
    private let recorder: RecordingHost<AppKitHost>
    private let renderer: Reconciler<RecordingHost<AppKitHost>>
    private let name: String
    private var passes = 0

    init(container: NSView, name: String) {
        self.name = name

        let appKit = AppKitHost()
        appKit.register(chipTag) { props in Chip(props: props) }

        // Wrapped so every mutation is visible. Nothing on screen distinguishes
        // a chip that was updated from one that was rebuilt, so without this
        // the claim that the reconciler is doing less work is unfalsifiable.
        recorder = RecordingHost(appKit)
        recorder.name(container, name)
        recorder.onEvent = { event in
            MigrationLog.note("      \(event.line)")
        }

        renderer = Reconciler(host: recorder, container: container)
    }

    func render(_ specs: [ChipSpec]) {
        passes += 1
        recorder.resetTally()

        MigrationLog.note("\(name) render #\(passes) — \(specs.count) chips")
        renderer.render(ChipRow(specs: specs).asElement())

        let tally = recorder.tally
        MigrationLog.note("  → " + (tally.isEmpty ? "no host work at all" : tally.summary))
    }
}
