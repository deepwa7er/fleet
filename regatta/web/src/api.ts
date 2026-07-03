import { req } from "@fleet/ui/api";

/** Every course's step quantities must add up to exactly this — the game is
 * "vote on ten": a budget of ten whole units spent across activities. */
export const COURSE_TOTAL = 10;

export interface Category {
  id: number;
  name: string;
  sort_order: number;
  created_at: string;
}

export interface Activity {
  id: number;
  name: string;
  category_id: number;
  unit: string;
  sort_order: number;
  created_at: string;
}

export interface Step {
  position: number;
  activity_id: number;
  activity: string;
  category: string;
  unit: string;
  quantity: number;
}

export interface Proposal {
  id: number;
  title: string;
  author: string;
  created_at: string;
  votes: number;
  voted: boolean;
  steps: Step[];
}

export interface ActivityInput {
  name: string;
  category_id: number;
  unit: string;
}

export interface ProposalInput {
  title: string;
  author: string;
  steps: { activity_id: number; quantity: number }[];
}

export const api = {
  categories: () => req<Category[]>("GET", "/api/categories"),
  createCategory: (name: string) => req<Category>("POST", "/api/categories", { name }),
  renameCategory: (id: number, name: string) =>
    req<Category>("PUT", `/api/categories/${id}`, { name }),
  deleteCategory: (id: number) => req<void>("DELETE", `/api/categories/${id}`),

  activities: () => req<Activity[]>("GET", "/api/activities"),
  createActivity: (data: ActivityInput) => req<Activity>("POST", "/api/activities", data),
  deleteActivity: (id: number) => req<void>("DELETE", `/api/activities/${id}`),

  proposals: (voter: string) =>
    req<Proposal[]>(
      "GET",
      voter ? `/api/proposals?voter=${encodeURIComponent(voter)}` : "/api/proposals",
    ),
  createProposal: (data: ProposalInput) => req<Proposal>("POST", "/api/proposals", data),
  deleteProposal: (id: number) => req<void>("DELETE", `/api/proposals/${id}`),

  castVote: (id: number, voter: string) =>
    req<Proposal>("POST", `/api/proposals/${id}/vote`, { voter }),
  retractVote: (id: number, voter: string) =>
    req<Proposal>("DELETE", `/api/proposals/${id}/vote`, { voter }),
};
