import { req } from "@fleet/ui/api";

export type Status = "wishlist" | "ordered" | "owned" | "skipped";

export const STATUSES: Status[] = ["wishlist", "ordered", "owned", "skipped"];

export interface Category {
  id: number;
  name: string;
  note: string | null;
  target_count: number | null;
  sort_order: number;
  created_at: string;
}

export interface Item {
  id: number;
  category_id: number;
  url: string;
  title: string | null;
  store: string | null;
  price_cents: number | null;
  image_url: string | null;
  status: Status;
  size: string | null;
  notes: string | null;
  sort_order: number;
  created_at: string;
  updated_at: string;
}

/** Editable fields shared by create and update (the server does a full replace). */
export interface ItemInput {
  category_id: number;
  url: string;
  title: string | null;
  store: string | null;
  price_cents: number | null;
  image_url: string | null;
  status: Status;
  size: string | null;
  notes: string | null;
}

export interface CategoryInput {
  name: string;
  note: string | null;
  target_count: number | null;
}

export interface LinkMeta {
  title: string | null;
  store: string | null;
  image_url: string | null;
  price_cents: number | null;
}

export const api = {
  categories: () => req<Category[]>("GET", "/api/categories"),
  createCategory: (data: CategoryInput) => req<Category>("POST", "/api/categories", data),
  updateCategory: (id: number, data: CategoryInput) =>
    req<Category>("PUT", `/api/categories/${id}`, data),
  deleteCategory: (id: number) => req<void>("DELETE", `/api/categories/${id}`),

  items: () => req<Item[]>("GET", "/api/items"),
  createItem: (data: ItemInput) => req<Item>("POST", "/api/items", data),
  updateItem: (id: number, data: ItemInput) => req<Item>("PUT", `/api/items/${id}`, data),
  setItemStatus: (id: number, status: Status) =>
    req<Item>("POST", `/api/items/${id}/status`, { status }),
  deleteItem: (id: number) => req<void>("DELETE", `/api/items/${id}`),

  fetchMeta: (url: string) => req<LinkMeta>("POST", "/api/fetch-meta", { url }),
};
