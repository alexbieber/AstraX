export type UsageRange = "today" | "7d" | "30d" | "all";

export type UsageTotals = {
  inputTokens: number;
  cachedInputTokens: number;
  outputTokens: number;
  reasoningTokens: number;
  totalTokens: number;
  sessionCount: number;
  usageEventCount: number;
};

export type UsageDay = UsageTotals & { date: string };
export type UsageModel = UsageTotals & { model: string };
export type UsageSession = UsageTotals & {
  id: string;
  title: string;
  model: string;
  startedAt: string;
  lastActiveAt: string;
};

export type UsageStatistics = {
  totals: UsageTotals;
  days: UsageDay[];
  models: UsageModel[];
  sessions: UsageSession[];
  availableModels: string[];
  coverage: {
    scannedFiles: number;
    matchedSessions: number;
    skippedFiles: number;
    warnings: string[];
    truncated: boolean;
  };
  rangeStart: string | null;
  rangeEnd: string;
  refreshedAt: string;
  timezone: string;
  sourceDirectories: string[];
};
