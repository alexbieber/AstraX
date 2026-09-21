import type { OfficialProfileSummary } from "./types";

type Options = {
  read: () => Promise<OfficialProfileSummary[]>;
  apply: (profiles: OfficialProfileSummary[]) => void;
  revision: () => number;
  canRead: () => boolean;
  isVisible: () => boolean;
  intervalMs?: number;
  schedule?: (callback: () => void, delay: number) => () => void;
};

// Only refreshes local display metadata. No full state reload, draft writes,
// network quota requests, or shared loading indicators belong in this monitor.
export function createOfficialProfileMonitor(options: Options) {
  const schedule = options.schedule ?? ((callback, delay) => {
    const timer = setTimeout(callback, delay);
    return () => clearTimeout(timer);
  });
  let stopped = false;
  let inFlight = false;
  let wakePending = false;
  let cancelTimer: (() => void) | undefined;

  function next() {
    cancelTimer?.();
    cancelTimer = undefined;
    if (!stopped && options.isVisible()) {
      cancelTimer = schedule(wake, options.intervalMs ?? 2_000);
    }
  }

  async function read() {
    if (stopped || !options.isVisible()) return;
    if (!options.canRead()) { next(); return; }
    inFlight = true;
    const revision = options.revision();
    try {
      const profiles = await options.read();
      if (!stopped && !wakePending && options.isVisible() && options.canRead()
        && options.revision() === revision) {
        options.apply(profiles);
      }
    } catch {
      // A temporary file/IPC failure must not blank the list or interrupt work.
    } finally {
      inFlight = false;
      if (wakePending) {
        wakePending = false;
        wake();
      } else {
        next();
      }
    }
  }

  function wake() {
    cancelTimer?.();
    cancelTimer = undefined;
    if (stopped) return;
    if (inFlight) { wakePending = true; return; }
    void read();
  }

  return {
    wake,
    stop() {
      stopped = true;
      wakePending = false;
      cancelTimer?.();
      cancelTimer = undefined;
    },
  };
}
