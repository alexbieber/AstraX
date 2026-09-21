import type { OfficialQuotaSnapshot, OfficialResetCreditsSnapshot } from "./officialQuotaTypes";

type QueryResult<T> = { data: T | null; busy: boolean; error: string };
export type OfficialQuotaState = {
  key: string | null;
  requestId: number;
  quota: QueryResult<OfficialQuotaSnapshot>;
  resetCredits: QueryResult<OfficialResetCreditsSnapshot>;
};

const emptyResult = () => ({ data: null, busy: false, error: "" });
export const initialOfficialQuotaState: OfficialQuotaState = {
  key: null, requestId: 0, quota: emptyResult(), resetCredits: emptyResult(),
};

type RequestIdentity = { key: string; requestId: number };
export type OfficialQuotaAction =
  | ({ type: "start"; retainResult: boolean } & RequestIdentity)
  | { type: "clear"; requestId: number }
  | ({ type: "quota-success"; data: OfficialQuotaSnapshot } & RequestIdentity)
  | ({ type: "reset-success"; data: OfficialResetCreditsSnapshot } & RequestIdentity)
  | ({ type: "quota-error" | "reset-error"; error: string } & RequestIdentity);

export function officialQuotaReducer(state: OfficialQuotaState, action: OfficialQuotaAction): OfficialQuotaState {
  if (action.type === "clear") {
    return action.requestId < state.requestId ? state : { ...initialOfficialQuotaState, requestId: action.requestId };
  }
  if (action.type === "start") {
    if (action.requestId < state.requestId) return state;
    const retain = action.retainResult && action.key === state.key;
    return {
      key: action.key, requestId: action.requestId,
      quota: { data: retain ? state.quota.data : null, busy: true, error: "" },
      resetCredits: { data: retain ? state.resetCredits.data : null, busy: true, error: "" },
    };
  }
  if (action.key !== state.key || action.requestId !== state.requestId) return state;
  if (action.type === "quota-success") return { ...state, quota: { data: action.data, busy: false, error: "" } };
  if (action.type === "reset-success") return { ...state, resetCredits: { data: action.data, busy: false, error: "" } };
  if (action.type === "quota-error") return { ...state, quota: { ...state.quota, busy: false, error: action.error } };
  return { ...state, resetCredits: { ...state.resetCredits, busy: false, error: action.error } };
}

type QuotaDetailsRequest = RequestIdentity & {
  profileId: string;
  loadQuota: () => Promise<OfficialQuotaSnapshot>;
  loadResetCredits: () => Promise<OfficialResetCreditsSnapshot>;
  dispatch: (action: OfficialQuotaAction) => void;
  isCurrent: () => boolean;
  mismatchMessage: string;
  invalidCountMessage: string;
};

// Start both native reads in the same turn. Each publishes its own result;
// waiting for the slower endpoint must never delay the other endpoint's UI.
export async function loadOfficialQuotaDetails(request: QuotaDetailsRequest): Promise<void> {
  const { key, requestId, profileId, dispatch, isCurrent } = request;
  const quota = (async () => {
    try {
      const data = await request.loadQuota();
      if (!isCurrent()) return;
      if (data.profileId !== profileId) throw new Error(request.mismatchMessage);
      dispatch({ type: "quota-success", key, requestId, data });
    } catch (cause) {
      if (isCurrent()) dispatch({ type: "quota-error", key, requestId, error: cause instanceof Error ? cause.message : String(cause) });
    }
  })();
  const resetCredits = (async () => {
    try {
      const data = await request.loadResetCredits();
      if (!isCurrent()) return;
      if (data.profileId !== profileId) throw new Error(request.mismatchMessage);
      if (!Number.isSafeInteger(data.availableCount) || data.availableCount < 0) throw new Error(request.invalidCountMessage);
      dispatch({ type: "reset-success", key, requestId, data });
    } catch (cause) {
      if (isCurrent()) dispatch({ type: "reset-error", key, requestId, error: cause instanceof Error ? cause.message : String(cause) });
    }
  })();
  await Promise.all([quota, resetCredits]);
}
