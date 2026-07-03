import { useEffect, useMemo, useState } from "react";
import {
  api,
  CATEGORIES,
  SEQUENCE_LEN,
  type Activity,
  type ActivityInput,
  type Category,
  type Proposal,
} from "./api";

const VOTER_KEY = "regatta-voter";

const categoryLabel = (c: Category) => CATEGORIES.find((x) => x.key === c)?.label ?? c;

/** Server order: votes desc, earlier proposal wins ties. Re-applied after a
 * vote so the board re-ranks in place. */
const byRank = (a: Proposal, b: Proposal) => b.votes - a.votes || a.id - b.id;

export function App() {
  const [activities, setActivities] = useState<Activity[]>([]);
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [voter, setVoter] = useState(() => localStorage.getItem(VOTER_KEY) ?? "");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showBuilder, setShowBuilder] = useState(false);
  const [showCatalog, setShowCatalog] = useState(false);

  const trimmedVoter = voter.trim();

  useEffect(() => {
    localStorage.setItem(VOTER_KEY, voter);
  }, [voter]);

  const load = async (asVoter: string) => {
    try {
      const [acts, props] = await Promise.all([api.activities(), api.proposals(asVoter)]);
      setActivities(acts);
      setProposals(props);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  // Reload whenever the voter identity settles so "voted" marks are theirs.
  useEffect(() => {
    const t = setTimeout(() => void load(trimmedVoter), trimmedVoter ? 300 : 0);
    return () => clearTimeout(t);
  }, [trimmedVoter]);

  const replaceProposal = (next: Proposal) =>
    setProposals((prev) => prev.map((p) => (p.id === next.id ? next : p)).sort(byRank));

  const toggleVote = async (p: Proposal) => {
    if (!trimmedVoter) return;
    try {
      const next = p.voted
        ? await api.retractVote(p.id, trimmedVoter)
        : await api.castVote(p.id, trimmedVoter);
      replaceProposal(next);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const removeProposal = async (p: Proposal) => {
    if (!confirm(`Delete "${p.title}"? Its votes go with it.`)) return;
    try {
      await api.deleteProposal(p.id);
      setProposals((prev) => prev.filter((x) => x.id !== p.id));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="app">
      <header>
        <h1>Regatta</h1>
        <span className="subtitle">ten-step courses — the crew votes</span>
        <div className="header-right">
          <label className="voter">
            <span>voting as</span>
            <input
              value={voter}
              onChange={(e) => setVoter(e.target.value)}
              placeholder="your name"
              size={12}
            />
          </label>
          <span className="docstamp">
            REG-001
            <br />
            SEQUENCE VOTING
          </span>
        </div>
      </header>

      <div className="toolbar">
        <span className="total">
          <b>{proposals.length}</b> proposal{proposals.length === 1 ? "" : "s"} on the board
        </span>
        <span className="spacer" />
        <button onClick={() => setShowCatalog(true)}>Catalog</button>
        <button className="primary" onClick={() => setShowBuilder(true)}>
          Propose a course
        </button>
      </div>

      {error && <div className="banner error">{error}</div>}

      {loading ? (
        <div className="loading">Loading the board…</div>
      ) : (
        <main className="main">
          {proposals.length === 0 && (
            <div className="empty-board">
              No proposals yet. Chart the first ten-step course.
            </div>
          )}
          {proposals.map((p, i) => (
            <ProposalCard
              key={p.id}
              proposal={p}
              rank={i + 1}
              canVote={trimmedVoter !== ""}
              onVote={() => void toggleVote(p)}
              onDelete={() => void removeProposal(p)}
            />
          ))}
        </main>
      )}

      {showBuilder && (
        <Builder
          activities={activities}
          author={trimmedVoter}
          onClose={() => setShowBuilder(false)}
          onCreated={(p) => {
            setProposals((prev) => [...prev, p].sort(byRank));
            setShowBuilder(false);
          }}
        />
      )}

      {showCatalog && (
        <Catalog
          activities={activities}
          onClose={() => setShowCatalog(false)}
          onChanged={setActivities}
        />
      )}
    </div>
  );
}

// ── one proposal on the board ────────────────────────────────────────────────

function ProposalCard(props: {
  proposal: Proposal;
  rank: number;
  canVote: boolean;
  onVote: () => void;
  onDelete: () => void;
}) {
  const { proposal: p, rank, canVote } = props;
  return (
    <section className={`proposal${rank === 1 && p.votes > 0 ? " leading" : ""}`}>
      <div className="proposal-head">
        <span className="rank">#{rank}</span>
        <div className="proposal-title">
          <h2>{p.title}</h2>
          <span className="byline">
            by {p.author} · {p.created_at.slice(0, 10)}
          </span>
        </div>
        <div className="tally">
          <span className="count">
            {p.votes} <small>vote{p.votes === 1 ? "" : "s"}</small>
          </span>
          <button
            className={p.voted ? "" : "primary"}
            disabled={!canVote}
            title={canVote ? undefined : "set your name to vote"}
            onClick={props.onVote}
          >
            {p.voted ? "Retract" : "Vote"}
          </button>
          <button className="ghost danger-hover" onClick={props.onDelete} title="delete proposal">
            ✕
          </button>
        </div>
      </div>
      <ol className="steps">
        {p.steps.map((s) => (
          <li key={s.position}>
            <span className="qty">
              {s.quantity} {s.unit}
            </span>
            <span className="act">{s.activity}</span>
            <span className="cat">{categoryLabel(s.category)}</span>
          </li>
        ))}
      </ol>
    </section>
  );
}

// ── the course builder ───────────────────────────────────────────────────────

interface DraftStep {
  activity_id: number | null;
  quantity: string;
}

const emptyDraft = (): DraftStep[] =>
  Array.from({ length: SEQUENCE_LEN }, () => ({ activity_id: null, quantity: "" }));

function Builder(props: {
  activities: Activity[];
  author: string;
  onClose: () => void;
  onCreated: (p: Proposal) => void;
}) {
  const [title, setTitle] = useState("");
  const [author, setAuthor] = useState(props.author);
  const [steps, setSteps] = useState<DraftStep[]>(emptyDraft);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const grouped = useMemo(
    () =>
      CATEGORIES.map((c) => ({
        ...c,
        activities: props.activities.filter((a) => a.category === c.key),
      })).filter((c) => c.activities.length > 0),
    [props.activities],
  );

  const setStep = (i: number, patch: Partial<DraftStep>) =>
    setSteps((prev) => prev.map((s, j) => (j === i ? { ...s, ...patch } : s)));

  const parsed = steps.map((s) => ({
    activity_id: s.activity_id,
    quantity: Number(s.quantity),
  }));
  const complete =
    title.trim() !== "" &&
    author.trim() !== "" &&
    parsed.every((s) => s.activity_id !== null && Number.isFinite(s.quantity) && s.quantity > 0);

  const unitFor = (id: number | null) => props.activities.find((a) => a.id === id)?.unit ?? "";

  const submit = async () => {
    if (!complete || busy) return;
    setBusy(true);
    try {
      const created = await api.createProposal({
        title: title.trim(),
        author: author.trim(),
        steps: parsed.map((s) => ({ activity_id: s.activity_id!, quantity: s.quantity })),
      });
      props.onCreated(created);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={props.onClose}>
      <div className="modal builder" onClick={(e) => e.stopPropagation()}>
        <h2>Propose a course — {SEQUENCE_LEN} steps</h2>
        <div className="field-row">
          <label className="field">
            <span>Title</span>
            <input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="the saturday gauntlet"
              autoFocus
            />
          </label>
          <label className="field">
            <span>Proposed by</span>
            <input value={author} onChange={(e) => setAuthor(e.target.value)} placeholder="you" />
          </label>
        </div>

        <div className="draft-steps">
          {steps.map((s, i) => (
            <div className="draft-step" key={i}>
              <span className="pos">{i + 1}</span>
              <input
                className="qty-input"
                type="number"
                min="0"
                step="any"
                inputMode="decimal"
                value={s.quantity}
                onChange={(e) => setStep(i, { quantity: e.target.value })}
                placeholder="qty"
                aria-label={`step ${i + 1} quantity`}
              />
              <span className="unit">{unitFor(s.activity_id) || "—"}</span>
              <select
                value={s.activity_id ?? ""}
                onChange={(e) =>
                  setStep(i, { activity_id: e.target.value ? Number(e.target.value) : null })
                }
                aria-label={`step ${i + 1} activity`}
              >
                <option value="">pick an activity…</option>
                {grouped.map((c) => (
                  <optgroup key={c.key} label={c.label}>
                    {c.activities.map((a) => (
                      <option key={a.id} value={a.id}>
                        {a.name}
                      </option>
                    ))}
                  </optgroup>
                ))}
              </select>
            </div>
          ))}
        </div>

        {error && <div className="banner error">{error}</div>}
        <div className="modal-actions">
          <span className="hint spacer">
            all {SEQUENCE_LEN} steps need an activity and a positive quantity
          </span>
          <button onClick={props.onClose}>Cancel</button>
          <button className="primary" disabled={!complete || busy} onClick={() => void submit()}>
            {busy ? "Charting…" : "Put it to a vote"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── the activity catalog editor ──────────────────────────────────────────────

function Catalog(props: {
  activities: Activity[];
  onClose: () => void;
  onChanged: (next: Activity[]) => void;
}) {
  const [draft, setDraft] = useState<ActivityInput>({ name: "", category: "misc", unit: "" });
  const [error, setError] = useState<string | null>(null);

  const add = async () => {
    if (!draft.name.trim() || !draft.unit.trim()) return;
    try {
      await api.createActivity({ ...draft, name: draft.name.trim(), unit: draft.unit.trim() });
      props.onChanged(await api.activities());
      setDraft({ name: "", category: draft.category, unit: "" });
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const remove = async (a: Activity) => {
    try {
      await api.deleteActivity(a.id);
      props.onChanged(props.activities.filter((x) => x.id !== a.id));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="modal-backdrop" onClick={props.onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>Activity catalog</h2>
        {CATEGORIES.map((c) => {
          const acts = props.activities.filter((a) => a.category === c.key);
          if (acts.length === 0) return null;
          return (
            <div className="catalog-group" key={c.key}>
              <h3>{c.label}</h3>
              <ul className="catalog-list">
                {acts.map((a) => (
                  <li key={a.id}>
                    <span className="act">{a.name}</span>
                    <span className="unit">per {a.unit}</span>
                    <button
                      className="ghost danger-hover"
                      onClick={() => void remove(a)}
                      title="delete activity (blocked while a proposal uses it)"
                    >
                      ✕
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          );
        })}

        <div className="catalog-add">
          <input
            value={draft.name}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            placeholder="new activity (e.g. Cartwheels)"
          />
          <input
            className="unit-input"
            value={draft.unit}
            onChange={(e) => setDraft({ ...draft, unit: e.target.value })}
            placeholder="unit"
          />
          <select
            value={draft.category}
            onChange={(e) => setDraft({ ...draft, category: e.target.value as Category })}
          >
            {CATEGORIES.map((c) => (
              <option key={c.key} value={c.key}>
                {c.label}
              </option>
            ))}
          </select>
          <button
            className="primary"
            disabled={!draft.name.trim() || !draft.unit.trim()}
            onClick={() => void add()}
          >
            Add
          </button>
        </div>

        {error && <div className="banner error">{error}</div>}
        <div className="modal-actions">
          <button onClick={props.onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}
