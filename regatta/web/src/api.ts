import { req } from "@fleet/ui/api";

/** Every course is exactly this many steps — the game is "vote on ten". */
export const SEQUENCE_LEN = 10;

export type Category = "foods" | "misc" | "physical" | "video-games";

export const CATEGORIES: { key: Category; label: string }[] = [
  { key: "foods", label: "Foods" },
  { key: "misc", label: "Misc" },
  { key: "physical", label: "Physical" },
  { key: "video-games", label: "Video games" },
];

export interface Activity {
  id: number;
  name: string;
  category: Category;
  unit: string;
  sort_order: number;
  created_at: string;
}

export interface Step {
  position: number;
  activity_id: number;
  activity: string;
  category: Category;
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
  category: Category;
  unit: string;
}

export interface ProposalInput {
  title: string;
  author: string;
  steps: { activity_id: number; quantity: number }[];
}

export const api = {
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
