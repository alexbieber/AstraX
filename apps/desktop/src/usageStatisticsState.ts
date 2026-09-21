import type { UsageRange, UsageStatistics } from "./usageTypes";

export type UsageQuery = { configDir: string; range: UsageRange; model: string };
export type UsageRecord = { query: UsageQuery; data: UsageStatistics };
export type UsageLoadState = {
  requestId: number;
  query: UsageQuery | null;
  record: UsageRecord | null;
  busy: boolean;
  error: string;
};

export const initialUsageLoadState: UsageLoadState = {
  requestId: 0, query: null, record: null, busy: false, error: "",
};

type UsageLoadAction =
  | { type: "start"; requestId: number; query: UsageQuery }
  | { type: "success"; requestId: number; data: UsageStatistics }
  | { type: "failure"; requestId: number; error: string }
  | { type: "cancel"; requestId: number };

export function sameUsageQuery(first: UsageQuery | null, second: UsageQuery): boolean {
  return first !== null && first.configDir === second.configDir
    && first.range === second.range && first.model === second.model;
}

export function usageLoadReducer(state: UsageLoadState, action: UsageLoadAction): UsageLoadState {
  if (action.type === "start") {
    if (action.requestId < state.requestId) return state;
    return {
      requestId: action.requestId, query: action.query, busy: true, error: "",
      // Range and model changes retain the rendered charts. A different Codex
      // directory must never inherit another directory's usage or model list.
      record: state.record?.query.configDir === action.query.configDir ? state.record : null,
    };
  }
  if (action.type === "cancel") {
    if (action.requestId < state.requestId) return state;
    return { ...state, requestId: action.requestId, busy: false };
  }
  if (action.requestId !== state.requestId || !state.busy || !state.query) return state;
  if (action.type === "success") {
    return { ...state, record: { query: state.query, data: action.data }, busy: false, error: "" };
  }
  return { ...state, busy: false, error: action.error };
}
