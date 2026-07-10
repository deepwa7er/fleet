import { req } from "@fleet/ui/api";

export interface Recipe {
  id: number;
  title: string;
  description: string | null;
  /** One ingredient per line. */
  ingredients: string;
  /** One step per line. */
  steps: string;
  tags: string[];
  servings: number | null;
  prep_minutes: number | null;
  cook_minutes: number | null;
  source_url: string | null;
  notes: string | null;
  created_at: string;
  updated_at: string;
}

/** Editable fields shared by create and update (the server does a full replace). */
export interface RecipeInput {
  title: string;
  description: string | null;
  ingredients: string;
  steps: string;
  tags: string[];
  servings: number | null;
  prep_minutes: number | null;
  cook_minutes: number | null;
  source_url: string | null;
  notes: string | null;
}

export const api = {
  recipes: () => req<Recipe[]>("GET", "/api/recipes"),
  recipe: (id: number) => req<Recipe>("GET", `/api/recipes/${id}`),
  createRecipe: (data: RecipeInput) => req<Recipe>("POST", "/api/recipes", data),
  updateRecipe: (id: number, data: RecipeInput) =>
    req<Recipe>("PUT", `/api/recipes/${id}`, data),
  deleteRecipe: (id: number) => req<void>("DELETE", `/api/recipes/${id}`),
};
