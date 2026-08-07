import AppKit
import Filament
import FilamentAppKit

/// Opt-in switches for work that is being migrated rather than replaced.
enum FeatureFlags {
    /// Drive the command panel's chip rows through Filament instead of the
    /// hand-rolled tear-down-and-rebuild path.
    ///
    ///     defaults write net.deepwa7er.tiler FilamentChips -bool YES
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
                // The title is the identity: a chip is the display or action it
                // names, so it keeps its view when the row is reordered and
                // loses it when the row genuinely changes.
                Node(chipTag, key: spec.title, [
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
    private let host = AppKitHost()
    private let renderer: Reconciler<AppKitHost>

    init(container: NSView) {
        host.register(chipTag) { props in Chip(props: props) }
        renderer = Reconciler(host: host, container: container)
    }

    func render(_ specs: [ChipSpec]) {
        renderer.render(ChipRow(specs: specs).asElement())
    }
}
