import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";
import { api } from "./api";
import type { Priority, State, Ticket, TicketDetail } from "./api";

const POLL_MS = 4000;

// Display order: states that need a human come first.
const GROUPS: { state: State; label: string; action: boolean }[] = [
  { state: "needs-input", label: "Needs input", action: true },
  { state: "blocked", label: "Blocked", action: true },
  { state: "in-review", label: "In review", action: true },
  { state: "open", label: "Open", action: false },
  { state: "in-progress", label: "In progress", action: false },
  { state: "done", label: "Done", action: false },
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

  if (ticket.state === "needs-input") {
    return (
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
    );
  }

  if (ticket.state === "blocked") {
    return (
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
    );
  }

  if (ticket.state === "in-review") {
    return (
      <div className="action-box">
        <h3>Review</h3>
        <p>Merge and deploy the PR yourself, then close this ticket.</p>
        <button
          className="primary"
          disabled={busy}
          onClick={() => run(() => api.done(ticket.id))}
        >
          Mark done
        </button>
      </div>
    );
  }

  return null;
}

function CreateForm({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => void;
}) {
  const [title, setTitle] = useState("");
  const [target, setTarget] = useState("");
  const [goal, setGoal] = useState("");
  const [priority, setPriority] = useState<Priority>("med");
  const [acceptance, setAcceptance] = useState("");
  const [constraints, setConstraints] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      await api.create({
        title,
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
          Service
          <input
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            placeholder="existing service name"
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
          Goal
          <textarea value={goal} onChange={(e) => setGoal(e.target.value)} rows={3} required />
        </label>
        <label>
          Acceptance criteria
          <textarea
            value={acceptance}
            onChange={(e) => setAcceptance(e.target.value)}
            rows={4}
            placeholder="- [ ] one criterion per line"
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

function fmt(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}
