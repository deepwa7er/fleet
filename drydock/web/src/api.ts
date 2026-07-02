import { req } from "@fleet/ui/api";

export type State =
  | "open"
  | "in-progress"
  | "needs-input"
  | "in-review"
  | "blocked"
  | "done"
  | "closed";

export type Priority = "high" | "med" | "low";

export interface Ticket {
  id: number;
  title: string;
  type: string;
  target: string;
  state: State;
  priority: Priority;
  goal: string;
  acceptance: string | null;
  constraints: string | null;
  branch: string | null;
  pr_url: string | null;
  created_at: string;
  updated_at: string;
}

export interface Comment {
  id: number;
  ticket_id: number;
  author: "worker" | "human";
  kind: "note" | "question" | "answer" | "report";
  body: string;
  created_at: string;
}

export interface Event {
  id: number;
  ticket_id: number;
  from_state: State | null;
  to_state: State;
  actor: "worker" | "human";
  detail: string | null;
  created_at: string;
}

export interface TicketDetail extends Ticket {
  comments: Comment[];
  events: Event[];
}

export type TicketType = "feature" | "investigate";

export interface Pulse {
  id: number;
  ticket_id: number | null;
  message: string;
  created_at: string;
}

export type WorkerStatus = "working" | "idle" | "stuck" | "offline" | "unknown";

export interface WorkerView {
  status: WorkerStatus;
  last_seen: string | null;
  seconds_since: number | null;
  active_ticket: Ticket | null;
  recent: Pulse[];
}

export interface NewTicket {
  title: string;
  type: TicketType;
  target: string;
  goal: string;
  priority: Priority;
  acceptance: string | null;
  constraints: string | null;
}

export const api = {
  list: (state?: State) =>
    req<Ticket[]>("GET", `/api/tickets${state ? `?state=${state}` : ""}`),
  detail: (id: number) => req<TicketDetail>("GET", `/api/tickets/${id}`),
  create: (data: NewTicket) => req<Ticket>("POST", "/api/tickets", data),
  answer: (id: number, body: string) =>
    req<Ticket>("POST", `/api/tickets/${id}/answer`, { body }),
  unblock: (id: number, note: string) =>
    req<Ticket>("POST", `/api/tickets/${id}/unblock`, { note }),
  done: (id: number) => req<Ticket>("POST", `/api/tickets/${id}/done`),
  close: (id: number, note: string) =>
    req<Ticket>("POST", `/api/tickets/${id}/close`, { note }),
  worker: () => req<WorkerView>("GET", "/api/worker"),
};
