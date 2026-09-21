import type { ConfigHealthReport } from "./configHealthTypes";

type Options = {
  read: () => Promise<ConfigHealthReport>;
  onReport: (report: ConfigHealthReport) => void;
  onProblem: (report: ConfigHealthReport) => void;
  onRecovered?: (report: ConfigHealthReport) => void;
  canRead: () => boolean;
  canNotify: () => boolean;
  isVisible: () => boolean;
  revision: () => number;
  intervalMs?: number;
  settleMs?: number;
  now?: () => number;
  schedule?: (callback: () => void, delay: number) => () => void;
};

// Confirm a broken file twice before notifying: an editor may briefly truncate
// or replace it. Reads are single-flight and never change shared loading state.
export function createConfigHealthMonitor(options: Options) {
  const schedule = options.schedule ?? ((callback, delay) => {
    const timer = setTimeout(callback, delay);
    return () => clearTimeout(timer);
  });
  const now = options.now ?? Date.now;
  const interval = options.intervalMs ?? 20_000;
  const settle = options.settleMs ?? 1_000;
  let stopped = false;
  let inFlight = false;
  let wakePending = false;
  let cancel: (() => void) | undefined;
  let candidate: { fingerprint: string; revision: number; at: number } | null = null;
  let recovery: { fingerprint: string; revision: number; at: number } | null = null;

  function next(delay = interval) {
    cancel?.();
    cancel = undefined;
    if (!stopped && options.isVisible()) cancel = schedule(wake, delay);
  }
  async function read() {
    if (stopped || !options.isVisible()) return;
    if (!options.canRead()) { next(); return; }
    inFlight = true;
    const revision = options.revision();
    let delay = interval;
    try {
      const report = await options.read();
      if (stopped || wakePending || revision !== options.revision() || !options.canRead() || !options.isVisible()) return;
      options.onReport(report);
      if (report.status === "healthy") {
        if (recovery?.fingerprint === report.fingerprint && recovery.revision === revision && now() - recovery.at >= settle) {
          options.onRecovered?.(report);
        } else if (!recovery || recovery.fingerprint !== report.fingerprint || recovery.revision !== revision) {
          recovery = { fingerprint: report.fingerprint, revision, at: now() };
        }
      } else recovery = null;
      if (report.status !== "issues" || !report.issues.length) {
        candidate = null;
        return;
      }
      if (!candidate || candidate.fingerprint !== report.fingerprint || candidate.revision !== revision) {
        candidate = { fingerprint: report.fingerprint, revision, at: now() };
        delay = settle;
      } else if (now() - candidate.at < settle) {
        delay = settle - (now() - candidate.at);
      } else if (options.canNotify()) {
        options.onProblem(report);
      }
    } catch {
      // A transient read/IPC failure is not evidence of a configuration error.
      candidate = null;
      recovery = null;
    } finally {
      inFlight = false;
      if (wakePending) { wakePending = false; wake(); } else next(delay);
    }
  }
  function wake() {
    cancel?.(); cancel = undefined;
    if (stopped) return;
    if (inFlight) { wakePending = true; return; }
    void read();
  }
  return { wake, stop() { stopped = true; cancel?.(); cancel = undefined; candidate = null; } };
}

// This is only notification deduplication, not a security or repair token.
function labelHash(value: string) {
  let first = 2166136261;
  let second = 5381;
  for (let index = 0; index < value.length; index += 1) {
    first = Math.imul(first ^ value.charCodeAt(index), 16777619);
    second = Math.imul(second, 33) ^ value.charCodeAt(index);
  }
  return `${(first >>> 0).toString(16)}-${(second >>> 0).toString(16)}`;
}
export function configHealthIssueKey(report: ConfigHealthReport) {
  return labelHash(JSON.stringify(report.issues.map(({ code, title, description }) => [code, title, description]).sort()));
}
type StorageLike = Pick<Storage, "getItem" | "setItem">;
export function createConfigHealthNoticeRegistry(storage?: StorageLike) {
  const storageKey = "codexx.configHealth.notified.v1";
  let entries: { scope: string; issue: string }[] = [];
  try {
    const saved: unknown = JSON.parse(storage?.getItem(storageKey) || "[]");
    if (Array.isArray(saved)) entries = saved.filter((value) => typeof value?.scope === "string" && typeof value?.issue === "string").slice(-32);
  } catch { /* Use in-memory suppression if browser storage is unavailable. */ }
  const scope = (report: ConfigHealthReport) => labelHash(report.codexDir.replace(/\\/g, "/"));
  const issue = configHealthIssueKey;
  const persist = () => { try { storage?.setItem(storageKey, JSON.stringify(entries)); } catch { /* Optional persistence. */ } };
  return {
    has(report: ConfigHealthReport) { return entries.some((entry) => entry.scope === scope(report) && entry.issue === issue(report)); },
    mark(report: ConfigHealthReport) {
      entries = [...entries.filter((entry) => entry.scope !== scope(report) || entry.issue !== issue(report)), { scope: scope(report), issue: issue(report) }].slice(-32);
      persist();
    },
    clear(report: ConfigHealthReport) {
      const remaining = entries.filter((entry) => entry.scope !== scope(report));
      if (remaining.length !== entries.length) { entries = remaining; persist(); }
    },
  };
}
