// The client's view of the wire protocol: one import site for types that are
// generated one-file-per-type.
//
// This barrel lives beside `gen/`, never inside it. `gen/` holds nothing that
// was not generated — skiff/tests/types.rs compares its whole contents against
// a fresh export and deletes anything unrecognised when regenerating, so a
// hand-written file in there would fail the gate and then be removed.
export type { Capabilities } from "./gen/Capabilities"
export type { ClientFrame } from "./gen/ClientFrame"
export type { Harness } from "./gen/Harness"
export type { ServerFrame } from "./gen/ServerFrame"
export type { SessionSummary } from "./gen/SessionSummary"
export type { SessionsView } from "./gen/SessionsView"
export type { SourceHealth } from "./gen/SourceHealth"
export type { ViewData } from "./gen/ViewData"
export type { ViewSpec } from "./gen/ViewSpec"
