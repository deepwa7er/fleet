import { useEffect, useMemo, useState } from "react";
import { api, STATUSES } from "./api";
import type {
  Category,
  CategoryInput,
  Item,
  ItemInput,
  LinkMeta,
  Status,
} from "./api";

// ── formatting helpers ─────────────────────────────────────────────────────

function formatPrice(cents: number | null): string {
  if (cents === null) return "";
  return `$${(cents / 100).toLocaleString("en-US", {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;
}

/** Dollars string → integer cents, mirroring the server's parser. */
function parseCents(input: string): number | null {
  const cleaned = input.replace(/[^0-9.]/g, "").replace(/^\.+|\.+$/g, "");
  if (cleaned === "") return null;
  const value = Number.parseFloat(cleaned);
  if (!Number.isFinite(value) || value < 0) return null;
  return Math.round(value * 100);
}

function centsToInput(cents: number | null): string {
  return cents === null ? "" : (cents / 100).toFixed(2);
}

function host(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

const STATUS_LABEL: Record<Status, string> = {
  wishlist: "Wishlist",
  ordered: "Ordered",
  owned: "Owned",
  skipped: "Skipped",
};

/** Clicking a status badge advances it along the shopping path and loops. */
const NEXT_STATUS: Record<Status, Status> = {
  wishlist: "ordered",
  ordered: "owned",
  owned: "skipped",
  skipped: "wishlist",
};

type Filter = Status | "all";

// ── app ─────────────────────────────────────────────────────────────────────

export function App() {
  const [categories, setCategories] = useState<Category[]>([]);
  const [items, setItems] = useState<Item[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<Filter>("all");

  const [itemModal, setItemModal] = useState<
    { mode: "create"; categoryId: number } | { mode: "edit"; item: Item } | null
  >(null);
  const [catModal, setCatModal] = useState<
    { mode: "create" } | { mode: "edit"; category: Category } | null
  >(null);

  const reload = async () => {
    try {
      const [cats, its] = await Promise.all([api.categories(), api.items()]);
      setCategories(cats);
      setItems(its);
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

  const run = async (fn: () => Promise<unknown>) => {
    try {
      await fn();
      await reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const itemsByCategory = useMemo(() => {
    const map = new Map<number, Item[]>();
    for (const it of items) {
      const list = map.get(it.category_id);
      if (list) list.push(it);
      else map.set(it.category_id, [it]);
    }
    return map;
  }, [items]);

  const statusCounts = useMemo(() => {
    const counts: Record<Filter, number> = {
      all: items.length,
      wishlist: 0,
      ordered: 0,
      owned: 0,
      skipped: 0,
    };
    for (const it of items) counts[it.status]++;
    return counts;
  }, [items]);

  // Overall build progress: owned pieces vs. the sum of category targets.
  const totalTarget = useMemo(
    () => categories.reduce((sum, c) => sum + (c.target_count ?? 0), 0),
    [categories],
  );
  const totalOwned = statusCounts.owned;

  return (
    <div className="app">
      <header>
        <h1>Clothes</h1>
        <span className="subtitle">Wardrobe · Classic Build</span>
        <div className="header-right">
          <span className="total">
            Owned <b>{totalOwned}</b> / {totalTarget} target
          </span>
          <span className="docstamp">
            DOC. CLO-001
            <br />
            REV. A
          </span>
        </div>
      </header>

      <div className="toolbar">
        <div className="filters">
          {(["all", ...STATUSES] as Filter[]).map((f) => (
            <button
              key={f}
              className={filter === f ? "on" : ""}
              onClick={() => setFilter(f)}
            >
              {f === "all" ? "All" : STATUS_LABEL[f]}
              <span className="fcount">{statusCounts[f]}</span>
            </button>
          ))}
        </div>
        <div className="spacer" />
        <button onClick={() => setCatModal({ mode: "create" })}>+ Category</button>
      </div>

      {error && (
        <div className="banner error" onClick={() => setError(null)}>
          {error} — click to dismiss
        </div>
      )}

      {loading ? (
        <div className="loading">Loading…</div>
      ) : (
        <main className="main">
          {categories.map((cat) => (
            <CategoryPanel
              key={cat.id}
              category={cat}
              items={itemsByCategory.get(cat.id) ?? []}
              filter={filter}
              onAddItem={() => setItemModal({ mode: "create", categoryId: cat.id })}
              onEditItem={(item) => setItemModal({ mode: "edit", item })}
              onEditCategory={() => setCatModal({ mode: "edit", category: cat })}
              onCycleStatus={(item) =>
                run(() => api.setItemStatus(item.id, NEXT_STATUS[item.status]))
              }
              onDeleteItem={(item) => {
                if (confirm(`Delete "${item.title ?? host(item.url)}"?`))
                  void run(() => api.deleteItem(item.id));
              }}
            />
          ))}
          {categories.length === 0 && (
            <div className="loading">
              No categories yet. Add one to start cataloguing.
            </div>
          )}
        </main>
      )}

      {itemModal && (
        <ItemModal
          categories={categories}
          initial={itemModal}
          onClose={() => setItemModal(null)}
          onSaved={async () => {
            setItemModal(null);
            await reload();
          }}
          onError={setError}
        />
      )}

      {catModal && (
        <CategoryModal
          initial={catModal}
          onClose={() => setCatModal(null)}
          onSaved={async () => {
            setCatModal(null);
            await reload();
          }}
          onDeleted={async () => {
            setCatModal(null);
            await reload();
          }}
          onError={setError}
        />
      )}
    </div>
  );
}

// ── category panel ──────────────────────────────────────────────────────────

function CategoryPanel({
  category,
  items,
  filter,
  onAddItem,
  onEditItem,
  onEditCategory,
  onCycleStatus,
  onDeleteItem,
}: {
  category: Category;
  items: Item[];
  filter: Filter;
  onAddItem: () => void;
  onEditItem: (item: Item) => void;
  onEditCategory: () => void;
  onCycleStatus: (item: Item) => void;
  onDeleteItem: (item: Item) => void;
}) {
  const owned = items.filter((i) => i.status === "owned").length;
  const target = category.target_count;
  const visible = filter === "all" ? items : items.filter((i) => i.status === filter);

  const pct =
    target && target > 0 ? Math.min(100, Math.round((owned / target) * 100)) : owned > 0 ? 100 : 0;
  const met = target != null && owned >= target && target > 0;

  return (
    <section className="category">
      <div className="cat-head">
        <h2>{category.name}</h2>
        {category.note && <span className="cat-note">{category.note}</span>}
        <div className="progress-and-actions cat-actions">
          <div className={`progress${met ? " met" : ""}`}>
            <span className="ratio">
              Owned <b>{owned}</b>
              {target != null ? ` / ${target}` : ""}
            </span>
            {target != null && target > 0 && (
              <span className="bar">
                <span style={{ width: `${pct}%` }} />
              </span>
            )}
          </div>
          <button className="ghost" title="Edit category" onClick={onEditCategory}>
            Edit
          </button>
          <button className="primary" onClick={onAddItem}>
            + Item
          </button>
        </div>
      </div>

      <table>
        <thead>
          <tr>
            <th>Status</th>
            <th>Item</th>
            <th>Store</th>
            <th>Size</th>
            <th className="num">Price</th>
            <th>Notes</th>
            <th className="num">Actions</th>
          </tr>
        </thead>
        <tbody>
          {visible.map((item) => (
            <tr key={item.id}>
              <td>
                <button
                  className={`status ${item.status}`}
                  title="Click to advance the shopping stage"
                  onClick={() => onCycleStatus(item)}
                >
                  {STATUS_LABEL[item.status]}
                </button>
              </td>
              <td>
                <div className="item-title">
                  {item.image_url && (
                    <img className="thumb" src={item.image_url} alt="" loading="lazy" />
                  )}
                  <div>
                    <a href={item.url} target="_blank" rel="noreferrer noopener">
                      {item.title ?? host(item.url)}
                    </a>
                    <div className="host">{host(item.url)}</div>
                  </div>
                </div>
              </td>
              <td>{item.store ?? ""}</td>
              <td>{item.size ?? ""}</td>
              <td className="num">{formatPrice(item.price_cents)}</td>
              <td>
                {item.notes && <span className="item-notes">{item.notes}</span>}
              </td>
              <td className="num">
                <div className="row-actions">
                  <button className="ghost" onClick={() => onEditItem(item)}>
                    Edit
                  </button>
                  <button className="ghost" onClick={() => onDeleteItem(item)}>
                    Del
                  </button>
                </div>
              </td>
            </tr>
          ))}
          {visible.length === 0 && (
            <tr className="empty-row">
              <td colSpan={7}>
                {items.length === 0
                  ? "No items yet — add a link to get started."
                  : `No ${filter} items in this category.`}
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </section>
  );
}

// ── item add/edit modal ─────────────────────────────────────────────────────

function ItemModal({
  categories,
  initial,
  onClose,
  onSaved,
  onError,
}: {
  categories: Category[];
  initial: { mode: "create"; categoryId: number } | { mode: "edit"; item: Item };
  onClose: () => void;
  onSaved: () => void;
  onError: (msg: string) => void;
}) {
  const existing = initial.mode === "edit" ? initial.item : null;
  const [categoryId, setCategoryId] = useState(
    existing ? existing.category_id : initial.mode === "create" ? initial.categoryId : 0,
  );
  const [url, setUrl] = useState(existing?.url ?? "");
  const [title, setTitle] = useState(existing?.title ?? "");
  const [store, setStore] = useState(existing?.store ?? "");
  const [price, setPrice] = useState(centsToInput(existing?.price_cents ?? null));
  const [imageUrl, setImageUrl] = useState(existing?.image_url ?? "");
  const [status, setStatus] = useState<Status>(existing?.status ?? "wishlist");
  const [size, setSize] = useState(existing?.size ?? "");
  const [notes, setNotes] = useState(existing?.notes ?? "");
  const [fetching, setFetching] = useState(false);
  const [saving, setSaving] = useState(false);

  const fetchMeta = async () => {
    if (url.trim() === "") return;
    setFetching(true);
    try {
      const meta: LinkMeta = await api.fetchMeta(url.trim());
      // Only fill blanks — never clobber what the user already typed.
      if (meta.title && title.trim() === "") setTitle(meta.title);
      if (meta.store && store.trim() === "") setStore(meta.store);
      if (meta.image_url && imageUrl.trim() === "") setImageUrl(meta.image_url);
      if (meta.price_cents != null && price.trim() === "")
        setPrice(centsToInput(meta.price_cents));
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setFetching(false);
    }
  };

  const submit = async () => {
    if (url.trim() === "") {
      onError("A link (URL) is required.");
      return;
    }
    const payload: ItemInput = {
      category_id: categoryId,
      url: url.trim(),
      title: title.trim() || null,
      store: store.trim() || null,
      price_cents: parseCents(price),
      image_url: imageUrl.trim() || null,
      status,
      size: size.trim() || null,
      notes: notes.trim() || null,
    };
    setSaving(true);
    try {
      if (existing) await api.updateItem(existing.id, payload);
      else await api.createItem(payload);
      onSaved();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>{existing ? "Edit Item" : "Add Item"}</h2>

        <label className="field">
          <span>Link</span>
          <div className="url-field">
            <input
              type="url"
              placeholder="https://…"
              value={url}
              autoFocus={!existing}
              onChange={(e) => setUrl(e.target.value)}
            />
            <button onClick={fetchMeta} disabled={fetching || url.trim() === ""}>
              {fetching ? "…" : "Fetch"}
            </button>
          </div>
          <span className="hint">
            Fetch pulls the title, store, image, and price from the page (best-effort).
          </span>
        </label>

        <label className="field">
          <span>Title</span>
          <input value={title} onChange={(e) => setTitle(e.target.value)} />
        </label>

        <div className="field-row">
          <label className="field">
            <span>Store / Brand</span>
            <input value={store} onChange={(e) => setStore(e.target.value)} />
          </label>
          <label className="field">
            <span>Price (USD)</span>
            <input
              inputMode="decimal"
              placeholder="0.00"
              value={price}
              onChange={(e) => setPrice(e.target.value)}
            />
          </label>
        </div>

        <div className="field-row">
          <label className="field">
            <span>Category</span>
            <select
              value={categoryId}
              onChange={(e) => setCategoryId(Number(e.target.value))}
            >
              {categories.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>Status</span>
            <select
              value={status}
              onChange={(e) => setStatus(e.target.value as Status)}
            >
              {STATUSES.map((s) => (
                <option key={s} value={s}>
                  {STATUS_LABEL[s]}
                </option>
              ))}
            </select>
          </label>
        </div>

        <div className="field-row">
          <label className="field">
            <span>Size</span>
            <input value={size} onChange={(e) => setSize(e.target.value)} />
          </label>
          <label className="field">
            <span>Image URL</span>
            <input value={imageUrl} onChange={(e) => setImageUrl(e.target.value)} />
          </label>
        </div>

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

// ── category add/edit modal ─────────────────────────────────────────────────

function CategoryModal({
  initial,
  onClose,
  onSaved,
  onDeleted,
  onError,
}: {
  initial: { mode: "create" } | { mode: "edit"; category: Category };
  onClose: () => void;
  onSaved: () => void;
  onDeleted: () => void;
  onError: (msg: string) => void;
}) {
  const existing = initial.mode === "edit" ? initial.category : null;
  const [name, setName] = useState(existing?.name ?? "");
  const [note, setNote] = useState(existing?.note ?? "");
  const [target, setTarget] = useState(
    existing?.target_count != null ? String(existing.target_count) : "",
  );
  const [saving, setSaving] = useState(false);

  const submit = async () => {
    if (name.trim() === "") {
      onError("A category name is required.");
      return;
    }
    const trimmedTarget = target.trim();
    const targetCount = trimmedTarget === "" ? null : Number.parseInt(trimmedTarget, 10);
    if (targetCount != null && (!Number.isFinite(targetCount) || targetCount < 0)) {
      onError("Target must be a non-negative whole number.");
      return;
    }
    const payload: CategoryInput = {
      name: name.trim(),
      note: note.trim() || null,
      target_count: targetCount,
    };
    setSaving(true);
    try {
      if (existing) await api.updateCategory(existing.id, payload);
      else await api.createCategory(payload);
      onSaved();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  };

  const remove = async () => {
    if (!existing) return;
    if (!confirm(`Delete "${existing.name}" and all its items? This cannot be undone.`))
      return;
    setSaving(true);
    try {
      await api.deleteCategory(existing.id);
      onDeleted();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>{existing ? "Edit Category" : "Add Category"}</h2>

        <label className="field">
          <span>Name</span>
          <input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </label>

        <label className="field">
          <span>Target count</span>
          <input
            inputMode="numeric"
            placeholder="e.g. 4 (leave blank for none)"
            value={target}
            onChange={(e) => setTarget(e.target.value)}
          />
          <span className="hint">How many pieces you want to own in this category.</span>
        </label>

        <label className="field">
          <span>Guidance note</span>
          <textarea rows={2} value={note} onChange={(e) => setNote(e.target.value)} />
        </label>

        <div className="modal-actions">
          {existing && (
            <button className="danger spacer" onClick={remove} disabled={saving}>
              Delete
            </button>
          )}
          <button onClick={onClose}>Cancel</button>
          <button className="primary" onClick={submit} disabled={saving}>
            {saving ? "Saving…" : existing ? "Save" : "Add"}
          </button>
        </div>
      </div>
    </div>
  );
}
