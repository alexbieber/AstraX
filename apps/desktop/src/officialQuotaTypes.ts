export type OfficialQuotaWindow = {
  id: string;
  windowSeconds: number | null;
  usedPercent: number | null;
  remainingPercent: number | null;
  resetsAt: string | null;
};

export type OfficialQuotaLimit = {
  id: string;
  name: string | null;
  allowed: boolean | null;
  limitReached: boolean | null;
  windows: OfficialQuotaWindow[];
};

export type OfficialQuotaSnapshot = {
  profileId: string;
  email: string | null;
  planType: string | null;
  limits: OfficialQuotaLimit[];
  checkedAt: string;
};

export type OfficialResetCreditsSnapshot = {
  profileId: string;
  availableCount: number;
  checkedAt: string;
};
