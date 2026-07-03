import { useEffect, useMemo, useState } from "react";
import {
  api,
  COURSE_STEPS,
  type Activity,
  type Category,
  type Proposal,
} from "./api";

const VOTER_KEY = "regatta-voter";

/** Server order: votes desc, earlier proposal wins ties. Re-applied after a
 * vote so the board re-ranks in place. */
const byRank = (a: Proposal, b: Proposal) => b.votes - a.votes || a.id - b.id;

export function App() {
  const [categories, setCategories] = useState<Category[]>([]);
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
      const [cats, acts, props] = await Promise.all([
        api.categories(),
        api.activities(),
        api.proposals(asVoter),
      ]);
      setCategories(cats);
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

  /** Re-fetch the catalog after the catalog modal changes it. */
  const reloadCatalog = async () => {
    try {
      const [cats, acts] = await Promise.all([api.categories(), api.activities()]);
      setCategories(cats);
      setActivities(acts);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

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
        <span className="subtitle">10 of one thing, 9 of another — the crew votes</span>
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
              No proposals yet. Chart the first countdown.
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
          categories={categories}
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
          categories={categories}
          activities={activities}
          onClose={() => setShowCatalog(false)}
          onChanged={() => void reloadCatalog()}
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
            <span className="cat">{s.category}</span>
          </li>
        ))}
      </ol>
    </section>
  );
}

// ── the course builder ───────────────────────────────────────────────────────

/** The quantity a countdown row demands: row 0 → 10, row 9 → 1. */
const quantityFor = (index: number) => COURSE_STEPS - index;

const emptyDraft = (): (number | null)[] => Array.from({ length: COURSE_STEPS }, () => null);

function Builder(props: {
  categories: Category[];
  activities: Activity[];
  author: string;
  onClose: () => void;
  onCreated: (p: Proposal) => void;
}) {
  const [title, setTitle] = useState("");
  const [author, setAuthor] = useState(props.author);
  const [picks, setPicks] = useState<(number | null)[]>(emptyDraft);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const grouped = useMemo(
    () =>
      props.categories
        .map((c) => ({
          ...c,
          activities: props.activities.filter((a) => a.category_id === c.id),
        }))
        .filter((c) => c.activities.length > 0),
    [props.categories, props.activities],
  );

  const setPick = (i: number, id: number | null) =>
    setPicks((prev) => prev.map((p, j) => (j === i ? id : p)));

  const chosen = new Set(picks.filter((p): p is number => p !== null));
  const distinct = chosen.size === picks.filter((p) => p !== null).length;
  const complete =
    title.trim() !== "" &&
    author.trim() !== "" &&
    picks.every((p) => p !== null) &&
    distinct;

  const unitFor = (id: number | null) => props.activities.find((a) => a.id === id)?.unit ?? "";

  const submit = async () => {
    if (!complete || busy) return;
    setBusy(true);
    try {
      const created = await api.createProposal({
        title: title.trim(),
        author: author.trim(),
        activities: picks as number[],
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
        <h2>Propose a course — the countdown</h2>
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
          {picks.map((pick, i) => (
            <div className="draft-step" key={i}>
              <span className="countdown-qty">{quantityFor(i)} ×</span>
              <select
                value={pick ?? ""}
                onChange={(e) => setPick(i, e.target.value ? Number(e.target.value) : null)}
                aria-label={`activity done ${quantityFor(i)} times`}
              >
                <option value="">pick an activity…</option>
                {grouped.map((c) => (
                  <optgroup key={c.id} label={c.name}>
                    {c.activities.map((a) => (
                      <option key={a.id} value={a.id} disabled={a.id !== pick && chosen.has(a.id)}>
                        {a.name}
                      </option>
                    ))}
                  </optgroup>
                ))}
              </select>
              <span className="unit">{unitFor(pick) || "—"}</span>
            </div>
          ))}
        </div>

        {error && <div className="banner error">{error}</div>}
        <div className="modal-actions">
          <span className="hint spacer">
            ten different activities — the first is done 10 times, the last once
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

// ── the catalog editor (categories + activities) ─────────────────────────────

function Catalog(props: {
  categories: Category[];
  activities: Activity[];
  onClose: () => void;
  onChanged: () => void;
}) {
  const [newCategory, setNewCategory] = useState("");
  const [renaming, setRenaming] = useState<{ id: number; name: string } | null>(null);
  const [draft, setDraft] = useState({ name: "", unit: "", category_id: null as number | null });
  const [error, setError] = useState<string | null>(null);

  const run = async (op: () => Promise<unknown>) => {
    try {
      await op();
      props.onChanged();
      setError(null);
      return true;
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return false;
    }
  };

  const addCategory = async () => {
    if (!newCategory.trim()) return;
    if (await run(() => api.createCategory(newCategory.trim()))) setNewCategory("");
  };

  const saveRename = async () => {
    if (!renaming || !renaming.name.trim()) return;
    if (await run(() => api.renameCategory(renaming.id, renaming.name.trim()))) setRenaming(null);
  };

  const addActivity = async () => {
    if (!draft.name.trim() || !draft.unit.trim() || draft.category_id === null) return;
    const ok = await run(() =>
      api.createActivity({
        name: draft.name.trim(),
        unit: draft.unit.trim(),
        category_id: draft.category_id!,
      }),
    );
    if (ok) setDraft({ name: "", unit: "", category_id: draft.category_id });
  };

  return (
    <div className="modal-backdrop" onClick={props.onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>Catalog — categories &amp; activities</h2>
        <p className="hint">
          Keep activities easily quantifiable — counts, streaks, minutes, miles — so nobody has to
          judge whether it really happened.
        </p>

        {props.categories.map((c) => {
          const acts = props.activities.filter((a) => a.category_id === c.id);
          return (
            <div className="catalog-group" key={c.id}>
              <div className="catalog-head">
                {renaming?.id === c.id ? (
                  <>
                    <input
                      value={renaming.name}
                      onChange={(e) => setRenaming({ id: c.id, name: e.target.value })}
                      onKeyDown={(e) => e.key === "Enter" && void saveRename()}
                      autoFocus
                    />
                    <button onClick={() => void saveRename()}>Save</button>
                    <button className="ghost" onClick={() => setRenaming(null)}>
                      Cancel
                    </button>
                  </>
                ) : (
                  <>
                    <h3>{c.name}</h3>
                    <button className="ghost" onClick={() => setRenaming({ id: c.id, name: c.name })}>
                      rename
                    </button>
                    <button
                      className="ghost danger-hover"
                      onClick={() => void run(() => api.deleteCategory(c.id))}
                      title="delete category (blocked while it has activities)"
                    >
                      ✕
                    </button>
                  </>
                )}
              </div>
              {acts.length > 0 && (
                <ul className="catalog-list">
                  {acts.map((a) => (
                    <li key={a.id}>
                      <span className="act">{a.name}</span>
                      <span className="unit">per {a.unit}</span>
                      <button
                        className="ghost danger-hover"
                        onClick={() => void run(() => api.deleteActivity(a.id))}
                        title="delete activity (blocked while a proposal uses it)"
                      >
                        ✕
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          );
        })}

        <div className="catalog-add">
          <input
            value={newCategory}
            onChange={(e) => setNewCategory(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void addCategory()}
            placeholder="new category (e.g. Water sports)"
          />
          <button className="primary" disabled={!newCategory.trim()} onClick={() => void addCategory()}>
            Add category
          </button>
        </div>

        <div className="catalog-add">
          <input
            value={draft.name}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            placeholder="new activity (e.g. Cannonballs done)"
          />
          <input
            className="unit-input"
            value={draft.unit}
            onChange={(e) => setDraft({ ...draft, unit: e.target.value })}
            placeholder="unit"
          />
          <select
            value={draft.category_id ?? ""}
            onChange={(e) =>
              setDraft({ ...draft, category_id: e.target.value ? Number(e.target.value) : null })
            }
          >
            <option value="">category…</option>
            {props.categories.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
          <button
            className="primary"
            disabled={!draft.name.trim() || !draft.unit.trim() || draft.category_id === null}
            onClick={() => void addActivity()}
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
