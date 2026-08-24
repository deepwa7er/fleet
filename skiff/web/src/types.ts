// The client's view of the wire protocol: one import site for types that are
// generated one-file-per-type.
//
// This barrel lives beside `gen/`, never inside it. `gen/` holds nothing that
// was not generated — skiff/tests/types.rs compares its whole contents against
// a fresh export and deletes anything unrecognised when regenerating, so a
// hand-written file in there would fail the gate and then be removed.
export type { Block } from "./gen/Block"
export type { Capabilities } from "./gen/Capabilities"
export type { ClientFrame } from "./gen/ClientFrame"
export type { Command } from "./gen/Command"
export type { Harness } from "./gen/Harness"
export type { Inline } from "./gen/Inline"
export type { LiveState } from "./gen/LiveState"
export type { Message } from "./gen/Message"
export type { Part } from "./gen/Part"
export type { PendingPrompt } from "./gen/PendingPrompt"
export type { Role } from "./gen/Role"
export type { ServerFrame } from "./gen/ServerFrame"
export type { SessionSummary } from "./gen/SessionSummary"
export type { SessionView } from "./gen/SessionView"
export type { SessionsView } from "./gen/SessionsView"
export type { SourceHealth } from "./gen/SourceHealth"
export type { Token } from "./gen/Token"
export type { TokenClass } from "./gen/TokenClass"
export type { ToolStatus } from "./gen/ToolStatus"
export type { ViewData } from "./gen/ViewData"
export type { ViewSpec } from "./gen/ViewSpec"
