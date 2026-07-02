import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";
import { api } from "./api";
import type {
  Priority,
  State,
  Ticket,
  TicketDetail,
  TicketType,
  WorkerView,
} from "./api";

const POLL_MS = 4000;

// Display order: states that need a human come first.
const GROUPS: { state: State; label: string; action: boolean }[] = [
  { state: "needs-input", label: "Needs input", action: true },
  { state: "blocked", label: "Blocked", action: true },
  { state: "in-review", label: "In review", action: true },
  { state: "open", label: "Open", action: false },
  { state: "in-progress", label: "In progress", action: false },
  { state: "done", label: "Done", action: false },
  { state: "closed", label: "Closed", action: false },
];

export function App() {
  const [tickets, setTickets] = useState<Ticket[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setTickets(await api.list());
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, POLL_MS);
    return () => clearInterval(t);
  }, [refresh]);

  // Deep-link: `?t=<id>` opens that ticket on load, so links from elsewhere
  // (e.g. spyglass federated search) land on the right ticket.
  useEffect(() => {
    const t = new URLSearchParams(window.location.search).get("t");
    const id = t ? Number.parseInt(t, 10) : NaN;
    if (Number.isInteger(id)) setSelected(id);
  }, []);

  return (
    <div className="app">
      <header>
        <h1>Drydock</h1>
        <span className="subtitle">Fleet ticket queue · deepwa7er</span>
        <div className="header-right">
          <span className="docstamp">DOC. DD-001 · REV 0.1.0</span>
          <button className="primary" onClick={() => setCreating(true)}>
            New ticket
          </button>
        </div>
      </header>
      {error && <div className="banner error">{error}</div>}
      <WorkerPanel />
      <div className="layout">
        <aside className="board">
          {GROUPS.map((g) => {
            const items = tickets.filter((t) => t.state === g.state);
            if (items.length === 0) return null;
            return (
              <section key={g.state} className={g.action ? "group action" : "group"}>
                <h2>
                  {g.label} <span className="count">{items.length}</span>
                </h2>
                {items.map((t) => (
                  <TicketCard
                    key={t.id}
                    ticket={t}
                    active={t.id === selected}
                    onClick={() => setSelected(t.id)}
                  />
                ))}
              </section>
            );
          })}
        </aside>
        <main className="detail-pane">
          {selected !== null ? (
            <Detail
              id={selected}
              onChanged={refresh}
              onClose={() => setSelected(null)}
            />
          ) : (
            <p className="empty">Select a ticket.</p>
          )}
        </main>
      </div>
      {creating && (
        <CreateForm
          onClose={() => setCreating(false)}
          onCreated={() => {
            setCreating(false);
            refresh();
          }}
        />
      )}
    </div>
  );
}

function TicketCard({
  ticket,
  active,
  onClick,
}: {
  ticket: Ticket;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button className={active ? "card active" : "card"} onClick={onClick}>
      <span className={`pri pri-${ticket.priority}`}>{ticket.priority}</span>
      <span className="card-title">{ticket.title}</span>
      <span className="card-target">{ticket.target}</span>
    </button>
  );
}

function Detail({
  id,
  onChanged,
  onClose,
}: {
  id: number;
  onChanged: () => void;
  onClose: () => void;
}) {
  const [ticket, setTicket] = useState<TicketDetail | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setTicket(await api.detail(id));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [id]);

  useEffect(() => {
    load();
    const t = setInterval(load, POLL_MS);
    return () => clearInterval(t);
  }, [load]);

  if (error) return <div className="banner error">{error}</div>;
  if (!ticket) return <p className="empty">Loading…</p>;

  const afterAction = async () => {
    await load();
    onChanged();
  };

  return (
    <article className="detail">
      <div className="detail-head">
        <h2>
          #{ticket.id} {ticket.title}
        </h2>
        <button className="ghost" onClick={onClose}>
          ✕
        </button>
      </div>
      <div className="meta">
        <span className={`badge state-${ticket.state}`}>{ticket.state}</span>
        {ticket.type === "investigate" && (
          <span className="badge type-investigate">investigate</span>
        )}
        <span className={`pri pri-${ticket.priority}`}>{ticket.priority}</span>
        <span className="target">{ticket.target}</span>
        {ticket.branch && <code>{ticket.branch}</code>}
        {ticket.pr_url && (
          <a href={ticket.pr_url} target="_blank" rel="noreferrer">
            PR ↗
          </a>
        )}
      </div>

      <h3>Goal</h3>
      <p className="prose">{ticket.goal}</p>
      {ticket.acceptance && (
        <>
          <h3>Acceptance</h3>
          <pre className="prose">{ticket.acceptance}</pre>
        </>
      )}
      {ticket.constraints && (
        <>
          <h3>Constraints</h3>
          <pre className="prose">{ticket.constraints}</pre>
        </>
      )}

      <Actions ticket={ticket} onDone={afterAction} onError={setError} />

      <h3>Thread</h3>
      {ticket.comments.length === 0 && <p className="empty">No messages yet.</p>}
      <ul className="thread">
        {ticket.comments.map((c) => (
          <li key={c.id} className={`msg ${c.author} ${c.kind}`}>
            <div className="msg-head">
              <strong>{c.author}</strong>
              <span className="kind">{c.kind}</span>
              <time>{fmt(c.created_at)}</time>
            </div>
            <div className="prose">{c.body}</div>
          </li>
        ))}
      </ul>

      <details className="audit">
        <summary>History</summary>
        <ul>
          {ticket.events.map((e) => (
            <li key={e.id}>
              <time>{fmt(e.created_at)}</time> {e.from_state ?? "—"} →{" "}
              <strong>{e.to_state}</strong> by {e.actor}
              {e.detail ? ` (${e.detail})` : ""}
            </li>
          ))}
        </ul>
      </details>
    </article>
  );
}

/** The human actions available depend on the ticket's current state. */
function Actions({
  ticket,
  onDone,
  onError,
}: {
  ticket: TicketDetail;
  onDone: () => Promise<void>;
  onError: (msg: string) => void;
}) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await fn();
      setText("");
      await onDone();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  // done + closed are terminal: no actions remain.
  const terminal = ticket.state === "done" || ticket.state === "closed";

  return (
    <>
      {ticket.state === "needs-input" && (
        <form
          className="action-box"
          onSubmit={(e) => {
            e.preventDefault();
            if (text.trim()) run(() => api.answer(ticket.id, text));
          }}
        >
          <h3>Answer the worker</h3>
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="Your answer — the worker reads this on its next run."
            rows={3}
          />
          <button className="primary" disabled={busy || !text.trim()}>
            Send answer
          </button>
        </form>
      )}

      {ticket.state === "blocked" && (
        <form
          className="action-box"
          onSubmit={(e) => {
            e.preventDefault();
            run(() => api.unblock(ticket.id, text));
          }}
        >
          <h3>Unblock</h3>
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="Optional note on what you changed to unblock this."
            rows={3}
          />
          <button className="primary" disabled={busy}>
            Unblock (→ open)
          </button>
        </form>
      )}

      {ticket.state === "in-review" && (
        <div className="action-box">
          <h3>Review</h3>
          <p>
            {ticket.type === "investigate"
              ? "Read the worker's report below, then mark it done."
              : "Merge and deploy the PR yourself, then mark it done."}
          </p>
          <button
            className="primary"
            disabled={busy}
            onClick={() => run(() => api.done(ticket.id))}
          >
            Mark done
          </button>
        </div>
      )}

      {!terminal && (
        <div className="action-box danger-box">
          <h3>Abandon</h3>
          <p>Close this ticket without doing it — for junk or won't-do work.</p>
          <button
            className="danger"
            disabled={busy}
            onClick={() => {
              if (
                window.confirm(
                  "Close (abandon) this ticket? It won't be worked. This is different from Done.",
                )
              ) {
                run(() => api.close(ticket.id, ""));
              }
            }}
          >
            Close ticket
          </button>
        </div>
      )}
    </>
  );
}

function CreateForm({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => void;
}) {
  const [title, setTitle] = useState("");
  const [type, setType] = useState<TicketType>("feature");
  const [target, setTarget] = useState("");
  const [goal, setGoal] = useState("");
  const [priority, setPriority] = useState<Priority>("med");
  const [acceptance, setAcceptance] = useState("");
  const [constraints, setConstraints] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const investigate = type === "investigate";

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      await api.create({
        title,
        type,
        target,
        goal,
        priority,
        acceptance: acceptance.trim() || null,
        constraints: constraints.trim() || null,
      });
      onCreated();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <form className="modal" onClick={(e) => e.stopPropagation()} onSubmit={submit}>
        <h2>New ticket</h2>
        {error && <div className="banner error">{error}</div>}
        <label>
          Title
          <input value={title} onChange={(e) => setTitle(e.target.value)} required />
        </label>
        <label>
          Type
          <select value={type} onChange={(e) => setType(e.target.value as TicketType)}>
            <option value="feature">feature — build it &amp; open a PR</option>
            <option value="investigate">
              investigate — look into it &amp; report findings
            </option>
          </select>
        </label>
        <label>
          Service
          <input
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            placeholder={investigate ? "service / area to investigate" : "existing service name"}
            required
          />
        </label>
        <label>
          Priority
          <select
            value={priority}
            onChange={(e) => setPriority(e.target.value as Priority)}
          >
            <option value="high">high</option>
            <option value="med">med</option>
            <option value="low">low</option>
          </select>
        </label>
        <label>
          {investigate ? "What to investigate" : "Goal"}
          <textarea
            value={goal}
            onChange={(e) => setGoal(e.target.value)}
            rows={3}
            required
            placeholder={
              investigate ? "Describe the issue or question to look into." : undefined
            }
          />
        </label>
        <label>
          {investigate ? "Where to look (optional)" : "Acceptance criteria"}
          <textarea
            value={acceptance}
            onChange={(e) => setAcceptance(e.target.value)}
            rows={4}
            placeholder={
              investigate
                ? "Hints: files, logs, the suspected area."
                : "- [ ] one criterion per line"
            }
          />
        </label>
        <label>
          Constraints
          <textarea
            value={constraints}
            onChange={(e) => setConstraints(e.target.value)}
            rows={2}
          />
        </label>
        <div className="modal-actions">
          <button type="button" className="ghost" onClick={onClose}>
            Cancel
          </button>
          <button className="primary" disabled={busy}>
            Create
          </button>
        </div>
      </form>
    </div>
  );
}

/** Worker liveness strip — is the Claude Desktop worker progressing or stuck? */
function WorkerPanel() {
  const [w, setW] = useState<WorkerView | null>(null);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const v = await api.worker();
        if (alive) setW(v);
      } catch {
        /* keep the previous view on a transient error */
      }
    };
    load();
    const t = setInterval(load, POLL_MS);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  if (!w) return null;

  const t = w.active_ticket;
  const line =
    (w.status === "working" || w.status === "stuck") && t
      ? `${w.status === "stuck" ? "possibly stuck on" : "working"} #${t.id} ${t.title}`
      : w.status === "idle"
        ? "idle"
        : w.status === "offline"
          ? "offline — not running?"
          : "no activity recorded yet";

  const time =
    w.seconds_since == null
      ? ""
      : `${w.status === "working" ? "active" : "last seen"} ${ago(w.seconds_since)}`;

  return (
    <div className={`worker worker-${w.status}`}>
      <button className="worker-head" onClick={() => setOpen((o) => !o)}>
        <span className="worker-dot" />
        <span className="worker-label">Worker — {line}</span>
        <span className="worker-time">{time}</span>
        <span className="worker-toggle">{open ? "▾" : "▸"}</span>
      </button>
      {open && (
        <ul className="worker-feed">
          {w.recent.length === 0 && <li className="empty">No activity recorded.</li>}
          {w.recent.map((p) => (
            <li key={p.id}>
              <time>{fmt(p.created_at)}</time>
              {p.ticket_id != null && <span className="feed-ticket">#{p.ticket_id}</span>}
              <span className="feed-msg">{p.message}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ago(seconds: number): string {
  if (seconds < 60) return `${seconds}s ago`;
  const m = Math.floor(seconds / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

function fmt(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}
