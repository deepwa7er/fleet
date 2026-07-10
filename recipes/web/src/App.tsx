import { useEffect, useMemo, useState } from "react";
import { api } from "./api";
import type { Recipe, RecipeInput } from "./api";

// ── formatting helpers ─────────────────────────────────────────────────────

function formatMinutes(min: number | null): string {
  if (min === null) return "";
  if (min < 60) return `${min}m`;
  const h = Math.floor(min / 60);
  const rest = min % 60;
  return rest === 0 ? `${h}h` : `${h}h ${rest}m`;
}

/** Total time when either component is set; null when the recipe has no times. */
function totalMinutes(r: Recipe): number | null {
  if (r.prep_minutes === null && r.cook_minutes === null) return null;
  return (r.prep_minutes ?? 0) + (r.cook_minutes ?? 0);
}

function lines(text: string): string[] {
  return text.split("\n").filter((l) => l.trim() !== "");
}

function formatDate(iso: string): string {
  return iso.slice(0, 10);
}

function host(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

/** "15" → 15, blank → null. Rejecting garbage is the caller's job via NaN. */
function parseCount(input: string): number | null {
  const trimmed = input.trim();
  if (trimmed === "") return null;
  return Number.parseInt(trimmed, 10);
}

// ── hash routing ────────────────────────────────────────────────────────────
// Two views: the cookbook index (#/) and one recipe (#/recipe/<id>). Hash
// routing keeps recipes linkable without any server-side route table.

type Route = { view: "list" } | { view: "recipe"; id: number };

function parseRoute(hash: string): Route {
  const m = hash.match(/^#\/recipe\/(\d+)$/);
  if (m) return { view: "recipe", id: Number(m[1]) };
  return { view: "list" };
}

function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parseRoute(location.hash));
  useEffect(() => {
    const onChange = () => setRoute(parseRoute(location.hash));
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return route;
}

function gotoList() {
  location.hash = "#/";
}

function gotoRecipe(id: number) {
  location.hash = `#/recipe/${id}`;
}

// ── app ─────────────────────────────────────────────────────────────────────

export function App() {
  const route = useRoute();
  const [recipes, setRecipes] = useState<Recipe[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [query, setQuery] = useState("");
  const [tagFilter, setTagFilter] = useState<string | null>(null);
  const [modal, setModal] = useState<
    { mode: "create" } | { mode: "edit"; recipe: Recipe } | null
  >(null);

  const reload = async () => {
    try {
      setRecipes(await api.recipes());
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void reload();
  }, []);

  const allTags = useMemo(() => {
    const tags = new Set<string>();
    for (const r of recipes) for (const t of r.tags) tags.add(t);
    return [...tags].sort();
  }, [recipes]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    return recipes.filter((r) => {
      if (tagFilter && !r.tags.includes(tagFilter)) return false;
      if (q === "") return true;
      return (
        r.title.toLowerCase().includes(q) ||
        r.ingredients.toLowerCase().includes(q) ||
        r.tags.some((t) => t.includes(q))
      );
    });
  }, [recipes, query, tagFilter]);

  const current =
    route.view === "recipe" ? recipes.find((r) => r.id === route.id) ?? null : null;

  const deleteRecipe = async (recipe: Recipe) => {
    if (!confirm(`Delete "${recipe.title}"? This cannot be undone.`)) return;
    try {
      await api.deleteRecipe(recipe.id);
      gotoList();
      await reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="app">
      <header>
        <h1>
          {route.view === "list" ? (
            "Recipes"
          ) : (
            <a className="crumb" href="#/">
              Recipes
            </a>
          )}
        </h1>
        <span className="subtitle">Cookbook · Fleet Kitchen</span>
        <div className="header-right">
          <span className="total">
            <b>{recipes.length}</b> {recipes.length === 1 ? "recipe" : "recipes"}
          </span>
          <span className="docstamp">
            DOC. RCP-001
            <br />
            REV. A
          </span>
        </div>
      </header>

      {error && (
        <div className="banner error" onClick={() => setError(null)}>
          {error} — click to dismiss
        </div>
      )}

      {loading ? (
        <div className="loading">Loading…</div>
      ) : route.view === "list" ? (
        <>
          <div className="toolbar">
            <input
              className="search"
              placeholder="Search title, ingredient, tag…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            {allTags.length > 0 && (
              <div className="filters">
                <button
                  className={tagFilter === null ? "on" : ""}
                  onClick={() => setTagFilter(null)}
                >
                  All
                </button>
                {allTags.map((t) => (
                  <button
                    key={t}
                    className={tagFilter === t ? "on" : ""}
                    onClick={() => setTagFilter(tagFilter === t ? null : t)}
                  >
                    {t}
                  </button>
                ))}
              </div>
            )}
            <div className="spacer" />
            <button className="primary" onClick={() => setModal({ mode: "create" })}>
              + Recipe
            </button>
          </div>
          <main className="main">
            <RecipeTable recipes={visible} totalCount={recipes.length} />
          </main>
        </>
      ) : current ? (
        <main className="main">
          <RecipeView
            recipe={current}
            onEdit={() => setModal({ mode: "edit", recipe: current })}
            onDelete={() => void deleteRecipe(current)}
          />
        </main>
      ) : (
        <main className="main">
          <div className="loading">
            Recipe #{route.id} not found. <a href="#/">Back to the index.</a>
          </div>
        </main>
      )}

      {modal && (
        <RecipeModal
          initial={modal}
          onClose={() => setModal(null)}
          onSaved={async (saved) => {
            setModal(null);
            await reload();
            gotoRecipe(saved.id);
          }}
          onError={setError}
        />
      )}
    </div>
  );
}

// ── cookbook index ───────────────────────────────────────────────────────────

function RecipeTable({ recipes, totalCount }: { recipes: Recipe[]; totalCount: number }) {
  return (
    <table>
      <thead>
        <tr>
          <th>Recipe</th>
          <th>Tags</th>
          <th className="num">Serves</th>
          <th className="num">Time</th>
          <th className="num">Updated</th>
        </tr>
      </thead>
      <tbody>
        {recipes.map((r) => (
          <tr key={r.id} className="index-row" onClick={() => gotoRecipe(r.id)}>
            <td>
              <a
                className="recipe-link"
                href={`#/recipe/${r.id}`}
                onClick={(e) => e.stopPropagation()}
              >
                {r.title}
              </a>
              {r.description && <div className="description">{r.description}</div>}
            </td>
            <td>
              <span className="tags">
                {r.tags.map((t) => (
                  <span key={t} className="tag">
                    {t}
                  </span>
                ))}
              </span>
            </td>
            <td className="num">{r.servings ?? ""}</td>
            <td className="num">{formatMinutes(totalMinutes(r))}</td>
            <td className="num">{formatDate(r.updated_at)}</td>
          </tr>
        ))}
        {recipes.length === 0 && (
          <tr className="empty-row">
            <td colSpan={5}>
              {totalCount === 0
                ? "No recipes yet — add one to start the cookbook."
                : "No recipes match the current search."}
            </td>
          </tr>
        )}
      </tbody>
    </table>
  );
}

// ── single recipe ────────────────────────────────────────────────────────────

function RecipeView({
  recipe,
  onEdit,
  onDelete,
}: {
  recipe: Recipe;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <article className="recipe">
      <div className="recipe-head">
        <div className="recipe-title">
          <h2>{recipe.title}</h2>
          {recipe.description && <p className="description">{recipe.description}</p>}
          <div className="recipe-meta">
            {recipe.tags.map((t) => (
              <span key={t} className="tag">
                {t}
              </span>
            ))}
            {recipe.servings !== null && <span>Serves {recipe.servings}</span>}
            {recipe.prep_minutes !== null && (
              <span>Prep {formatMinutes(recipe.prep_minutes)}</span>
            )}
            {recipe.cook_minutes !== null && (
              <span>Cook {formatMinutes(recipe.cook_minutes)}</span>
            )}
            {recipe.source_url && (
              <a href={recipe.source_url} target="_blank" rel="noreferrer noopener">
                Source: {host(recipe.source_url)}
              </a>
            )}
          </div>
        </div>
        <div className="recipe-actions">
          <button className="ghost" onClick={onEdit}>
            Edit
          </button>
          <button className="ghost danger-ghost" onClick={onDelete}>
            Del
          </button>
        </div>
      </div>

      <div className="recipe-body">
        <section className="ingredients">
          <h3>Ingredients</h3>
          <ul>
            {lines(recipe.ingredients).map((ing, i) => (
              <li key={i}>{ing}</li>
            ))}
          </ul>
        </section>
        <section className="steps">
          <h3>Steps</h3>
          <ol>
            {lines(recipe.steps).map((step, i) => (
              <li key={i}>{step}</li>
            ))}
          </ol>
          {recipe.notes && (
            <>
              <h3>Notes</h3>
              <p className="notes">{recipe.notes}</p>
            </>
          )}
        </section>
      </div>

      <div className="recipe-foot">
        Added {formatDate(recipe.created_at)}
        {recipe.updated_at !== recipe.created_at &&
          ` · updated ${formatDate(recipe.updated_at)}`}
      </div>
    </article>
  );
}

// ── add / edit modal ─────────────────────────────────────────────────────────

function RecipeModal({
  initial,
  onClose,
  onSaved,
  onError,
}: {
  initial: { mode: "create" } | { mode: "edit"; recipe: Recipe };
  onClose: () => void;
  onSaved: (saved: Recipe) => void;
  onError: (msg: string) => void;
}) {
  const existing = initial.mode === "edit" ? initial.recipe : null;
  const [title, setTitle] = useState(existing?.title ?? "");
  const [description, setDescription] = useState(existing?.description ?? "");
  const [ingredients, setIngredients] = useState(existing?.ingredients ?? "");
  const [steps, setSteps] = useState(existing?.steps ?? "");
  const [tags, setTags] = useState(existing?.tags.join(", ") ?? "");
  const [servings, setServings] = useState(existing?.servings?.toString() ?? "");
  const [prep, setPrep] = useState(existing?.prep_minutes?.toString() ?? "");
  const [cook, setCook] = useState(existing?.cook_minutes?.toString() ?? "");
  const [sourceUrl, setSourceUrl] = useState(existing?.source_url ?? "");
  const [notes, setNotes] = useState(existing?.notes ?? "");
  const [saving, setSaving] = useState(false);

  const submit = async () => {
    if (title.trim() === "") {
      onError("A title is required.");
      return;
    }
    if (ingredients.trim() === "") {
      onError("At least one ingredient is required.");
      return;
    }
    if (steps.trim() === "") {
      onError("At least one step is required.");
      return;
    }
    const counts = { servings: parseCount(servings), prep: parseCount(prep), cook: parseCount(cook) };
    for (const [field, value] of Object.entries(counts)) {
      if (value !== null && (!Number.isFinite(value) || value < 0)) {
        onError(`${field} must be a non-negative whole number.`);
        return;
      }
    }
    const payload: RecipeInput = {
      title: title.trim(),
      description: description.trim() || null,
      ingredients,
      steps,
      tags: tags.split(",").map((t) => t.trim()).filter((t) => t !== ""),
      servings: counts.servings,
      prep_minutes: counts.prep,
      cook_minutes: counts.cook,
      source_url: sourceUrl.trim() || null,
      notes: notes.trim() || null,
    };
    setSaving(true);
    try {
      const saved = existing
        ? await api.updateRecipe(existing.id, payload)
        : await api.createRecipe(payload);
      onSaved(saved);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>{existing ? "Edit Recipe" : "Add Recipe"}</h2>

        <label className="field">
          <span>Title</span>
          <input value={title} autoFocus={!existing} onChange={(e) => setTitle(e.target.value)} />
        </label>

        <label className="field">
          <span>Description</span>
          <input
            placeholder="One line on what this is"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
          />
        </label>

        <label className="field">
          <span>Ingredients</span>
          <textarea
            rows={6}
            placeholder={"One per line, e.g.\n200g spaghetti\n2 cloves garlic"}
            value={ingredients}
            onChange={(e) => setIngredients(e.target.value)}
          />
        </label>

        <label className="field">
          <span>Steps</span>
          <textarea
            rows={8}
            placeholder={"One per line, e.g.\nBoil the pasta.\nSlice the garlic."}
            value={steps}
            onChange={(e) => setSteps(e.target.value)}
          />
        </label>

        <div className="field-row three">
          <label className="field">
            <span>Serves</span>
            <input inputMode="numeric" value={servings} onChange={(e) => setServings(e.target.value)} />
          </label>
          <label className="field">
            <span>Prep (min)</span>
            <input inputMode="numeric" value={prep} onChange={(e) => setPrep(e.target.value)} />
          </label>
          <label className="field">
            <span>Cook (min)</span>
            <input inputMode="numeric" value={cook} onChange={(e) => setCook(e.target.value)} />
          </label>
        </div>

        <label className="field">
          <span>Tags</span>
          <input
            placeholder="comma-separated, e.g. dinner, pasta"
            value={tags}
            onChange={(e) => setTags(e.target.value)}
          />
        </label>

        <label className="field">
          <span>Source URL</span>
          <input type="url" placeholder="https://…" value={sourceUrl} onChange={(e) => setSourceUrl(e.target.value)} />
        </label>

        <label className="field">
          <span>Notes</span>
          <textarea rows={2} value={notes} onChange={(e) => setNotes(e.target.value)} />
        </label>

        <div className="modal-actions">
          <button onClick={onClose}>Cancel</button>
          <button className="primary" onClick={submit} disabled={saving}>
            {saving ? "Saving…" : existing ? "Save" : "Add"}
          </button>
        </div>
      </div>
    </div>
  );
}
