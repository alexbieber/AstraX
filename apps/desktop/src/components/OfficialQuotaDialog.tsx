import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AlertCircle, CheckCircle2, ChevronDown, Clock3, Gauge, Loader2, RefreshCw, Sparkles, Ticket } from "lucide-react";
import { Button, ModalShell } from "./ui";
import { OfficialAccountBadge } from "./OfficialAccountBadge";
import type { OfficialQuotaLimit, OfficialQuotaSnapshot, OfficialQuotaWindow, OfficialResetCreditsSnapshot } from "../officialQuotaTypes";
import { initialOfficialQuotaState, loadOfficialQuotaDetails, officialQuotaReducer } from "../officialQuotaState";
import { getOfficialPlan } from "../officialPlan";
import "../styles/official-quota.css";

type Language = "zh" | "en";
type QuotaProfile = { id: string; name: string; email: string | null; planType?: string | null };

export type OfficialQuotaDialogProps = {
  lang: Language;
  configDir: string;
  profile: QuotaProfile | null;
  onClose: () => void;
};

function getCopy(lang: Language) {
  return {
    title: "Official Codex quota", close: "Close", refresh: "Refresh quota", refreshing: "Checking",
    loading: "Checking this account’s available quota…", loadingHint: "You can close this window and check again later.",
    error: "Unable to check quota", previous: "Refresh failed. The last available results are shown.",
    accountUnknown: "Account email not provided", planUnknown: "Plan not provided", remaining: "Remaining", used: "Used",
    resetUnknown: "Reset time not provided", resetDue: "Reset time reached. Refresh to check.", percentUnknown: "Percentage not provided",
    windowUnknown: "Unknown window", weekly: "Weekly quota", main: "General quota", others: "Other quotas",
    empty: "General quota not returned", emptyHint: "The service did not provide this account’s general quota. Try refreshing later.",
    noWindow: "Quota window not provided", noWindowHint: "The service did not provide percentages or reset times for this quota.",
    unavailable: "Currently unavailable", reached: "Limit reached", available: "Available", checked: "Checked",
    codeReview: "Code review", mismatch: "The returned quota belongs to a different account. Please try again.",
    snapshotHint: "Quota reflects the last query. Refresh after the countdown ends to confirm availability.",
    resets: "Available resets", resetLoading: "Checking", resetError: "Unable to check available resets",
    resetPrevious: "Reset count refresh failed. The last result is shown.", resetInvalid: "The service did not return a valid available reset count.",
  };
}

type QuotaCopy = ReturnType<typeof getCopy>;

function finitePercent(value: number | null): number | null {
  return value !== null && Number.isFinite(value) ? Math.min(100, Math.max(0, value)) : null;
}

function formatPercent(value: number): string {
  return Number(value.toFixed(1)).toString();
}

function formatWindow(seconds: number | null, lang: Language, copy: QuotaCopy): string {
  if (seconds === null || !Number.isFinite(seconds) || seconds <= 0) return copy.windowUnknown;
  if (seconds === 604_800) return copy.weekly;
  let rest = Math.floor(seconds);
  const values: string[] = [];
  const units: [number, string][] = [[86_400, "d"], [3_600, "h"], [60, "m"], [1, "s"]];
  for (const [size, en] of units) {
    const value = Math.floor(rest / size);
    if (value) values.push(`${value}${en}`);
    rest %= size;
  }
  const duration = values.join(" ");
  return `${duration} quota`;
}

function formatReset(resetsAt: string | null, now: number, lang: Language, copy: QuotaCopy): { relative: string; exact: string | null; overdue: boolean } {
  const time = resetsAt ? Date.parse(resetsAt) : Number.NaN;
  if (!Number.isFinite(time)) return { relative: copy.resetUnknown, exact: null, overdue: false };
  const exact = new Date(time).toLocaleString("en-US", {
    year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false,
  });
  if (time <= now) return { relative: copy.resetDue, exact, overdue: true };
  const minutes = Math.ceil((time - now) / 60_000);
  const days = Math.floor(minutes / 1_440);
  const hours = Math.floor(minutes % 1_440 / 60);
  const remainder = minutes % 60;
  let duration: string;
  duration = days ? `${days}d${hours ? ` ${hours}h` : ""}` : hours ? `${hours}h${remainder ? ` ${remainder}m` : ""}` : `${minutes}m`;
  return { relative: `Resets in ${duration}`, exact, overdue: false };
}

function limitLabel(limit: OfficialQuotaLimit, copy: QuotaCopy): string {
  if (limit.id === "main") return copy.main;
  if (limit.id === "code_review" || limit.id === "code-review") return copy.codeReview;
  return limit.name?.trim() || limit.id;
}

function QuotaWindow({ window, lang, copy, now, compact = false }: { window: OfficialQuotaWindow; lang: Language; copy: QuotaCopy; now: number; compact?: boolean }) {
  const remaining = finitePercent(window.remainingPercent);
  const used = finitePercent(window.usedPercent);
  const reset = formatReset(window.resetsAt, now, lang, copy);
  const label = formatWindow(window.windowSeconds, lang, copy);
  const low = remaining !== null && remaining <= 10;

  return <article className={`cx-quota-window${compact ? " cx-quota-window--compact" : ""}${low ? " cx-quota-window--low" : ""}`}>
    <div className="cx-quota-window-summary">
      <div className="cx-quota-window-heading"><strong>{label}</strong></div>
      <div className="cx-quota-window-value"><span>{remaining === null ? "—" : formatPercent(remaining)}</span>{remaining !== null && <small>%</small>}</div>
    </div>
    <div className="cx-quota-window-progress">
      <span>{copy.remaining}</span>
      <div className={`cx-quota-meter${remaining === null ? " cx-quota-meter--unknown" : ""}`} role="progressbar" aria-label={`${label} · ${copy.remaining}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={remaining ?? undefined} aria-valuetext={remaining === null ? copy.percentUnknown : `${formatPercent(remaining)}%`}>
        {remaining !== null && <span style={{ width: `${remaining}%` }} />}
      </div>
      <div className="cx-quota-window-used" title={used === null ? copy.percentUnknown : undefined}>{copy.used} {used === null ? "—" : `${formatPercent(used)}%`}</div>
    </div>
    <div className={`cx-quota-reset${reset.overdue ? " cx-quota-reset--due" : ""}`}>
      <Clock3 size={12} aria-hidden="true" /><div><span>{reset.relative}</span>{reset.exact && <time dateTime={window.resetsAt!}>{reset.exact}</time>}</div>
    </div>
  </article>;
}

function QuotaLimit({ limit, lang, copy, now, compact = false }: { limit: OfficialQuotaLimit; lang: Language; copy: QuotaCopy; now: number; compact?: boolean }) {
  const status = limit.limitReached === true ? copy.reached : limit.allowed === false ? copy.unavailable : limit.allowed === true ? copy.available : null;
  const windowOrder = (window: OfficialQuotaWindow) => window.windowSeconds !== null && Number.isFinite(window.windowSeconds) && window.windowSeconds > 0 ? window.windowSeconds : Number.POSITIVE_INFINITY;
  const windows = [...limit.windows].sort((a, b) => windowOrder(a) - windowOrder(b));
  return <section className={`cx-quota-limit${compact ? " cx-quota-limit--compact" : ""}`}>
    <div className="cx-quota-limit-heading"><h3>{limitLabel(limit, copy)}</h3>{status && <span className={`cx-quota-status${limit.limitReached === true || limit.allowed === false ? " cx-quota-status--unavailable" : ""}`}>{limit.allowed === true && limit.limitReached !== true && <CheckCircle2 size={11} aria-hidden="true" />}{status}</span>}</div>
    {windows.length > 0 ? <div className="cx-quota-windows">{windows.map((window) => <QuotaWindow key={window.id} window={window} lang={lang} copy={copy} now={now} compact={compact} />)}</div> : <div className="cx-quota-no-window"><strong>{copy.noWindow}</strong><p>{copy.noWindowHint}</p></div>}
  </section>;
}

export function OfficialQuotaDialog({ lang, configDir, profile, onClose }: OfficialQuotaDialogProps) {
  const copy = getCopy(lang);
  const profileId = profile?.id ?? null;
  const queryKey = JSON.stringify([configDir, profileId]);
  const [state, dispatch] = useReducer(officialQuotaReducer, initialOfficialQuotaState);
  const [now, setNow] = useState(Date.now);
  const requestId = useRef(0);
  const currentKey = useRef(queryKey);
  currentKey.current = queryKey;
  const current = state.key === queryKey ? state : initialOfficialQuotaState;
  const { data, busy: quotaBusy, error } = current.quota;
  const { data: resetCredits, busy: resetBusy, error: resetError } = current.resetCredits;
  const busy = quotaBusy || resetBusy;

  const load = useCallback(async (retainResult = false) => {
    if (!profileId) return;
    const request = ++requestId.current;
    dispatch({ type: "start", key: queryKey, requestId: request, retainResult });
    await loadOfficialQuotaDetails({
      key: queryKey, requestId: request, profileId,
      loadQuota: () => invoke<OfficialQuotaSnapshot>("get_official_profile_quota", { configDir: configDir || null, profileId }),
      loadResetCredits: () => invoke<OfficialResetCreditsSnapshot>("get_official_profile_reset_credits", { configDir: configDir || null, profileId }),
      dispatch,
      isCurrent: () => request === requestId.current && currentKey.current === queryKey,
      mismatchMessage: copy.mismatch, invalidCountMessage: copy.resetInvalid,
    });
  }, [configDir, profileId, queryKey, copy.mismatch, copy.resetInvalid]);

  useEffect(() => {
    if (!profileId) { dispatch({ type: "clear", requestId: ++requestId.current }); return; }
    void load();
    return () => { requestId.current += 1; };
  }, [profileId, load]);

  useEffect(() => {
    if (!profileId) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, [profileId, configDir]);

  useEffect(() => { if (data) setNow(Date.now()); }, [data]);

  const close = () => {
    dispatch({ type: "clear", requestId: ++requestId.current });
    onClose();
  };
  const main = data?.limits.find((limit) => limit.id === "main");
  const others = data?.limits.filter((limit) => limit.id !== "main") ?? [];
  const email = data?.email || profile?.email;
  const checked = data?.checkedAt && Number.isFinite(Date.parse(data.checkedAt)) ? new Date(data.checkedAt).toLocaleString("en-US", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false }) : null;

  return <ModalShell open={Boolean(profile)} onClose={close} title={copy.title} description={profile?.name} closeLabel={copy.close} size="md" className="cx-official-quota-dialog" bodyClassName="cx-official-quota-body" footer={
    <><Button variant="secondary" onClick={close}>{copy.close}</Button><Button variant="primary" icon={busy ? <Loader2 size={14} className="cx-quota-spin" /> : <RefreshCw size={14} />} disabled={busy} onClick={() => void load(true)}>{busy ? copy.refreshing : copy.refresh}</Button></>
  }>
    <div className="cx-quota-account"><div className="cx-quota-account-icon" aria-hidden="true"><Gauge size={21} strokeWidth={1.7} /></div><div className="cx-quota-account-copy"><div><Sparkles size={12} aria-hidden="true" /><strong>{getOfficialPlan(data?.planType ?? profile?.planType, lang)?.label || copy.planUnknown}</strong></div><OfficialAccountBadge lang={lang} email={email} hasAuth canQueryQuota className="cx-quota-account-email" /></div></div>
    <section className={`cx-quota-reset-credits${resetError ? " cx-quota-reset-credits--error" : ""}`} aria-label={copy.resets} aria-busy={resetBusy}>
      <div className="cx-quota-reset-credits-row">
        <span><Ticket size={16} aria-hidden="true" />{copy.resets}</span>
        <div aria-live="polite">
          {resetBusy && <Loader2 size={13} className="cx-quota-spin" aria-hidden="true" />}
          {resetCredits ? <strong>{resetCredits.availableCount}<small>{resetCredits.availableCount === 1 ? "reset" : "resets"}</small></strong> : <span className="cx-quota-reset-credits-status">{resetBusy ? copy.resetLoading : "—"}</span>}
        </div>
      </div>
      {resetError && <p role="alert"><AlertCircle size={12} aria-hidden="true" /><span><strong>{resetCredits ? copy.resetPrevious : copy.resetError}</strong><span>{resetError}</span></span></p>}
    </section>
    {error && <div className="cx-quota-error" role="alert"><AlertCircle size={17} aria-hidden="true" /><div><strong>{copy.error}</strong>{data && <p>{copy.previous}</p>}<p>{error}</p></div></div>}
    {quotaBusy && !data && <div className="cx-quota-loading" role="status"><Loader2 size={24} className="cx-quota-spin" aria-hidden="true" /><strong>{copy.loading}</strong><p>{copy.loadingHint}</p><div className="cx-quota-loading-bars" aria-hidden="true"><span /><span /></div></div>}
    {data && <>
      {main ? <QuotaLimit limit={main} lang={lang} copy={copy} now={now} /> : <div className="cx-quota-empty"><Gauge size={26} aria-hidden="true" /><strong>{copy.empty}</strong><p>{copy.emptyHint}</p></div>}
      {others.length > 0 && <details className="cx-quota-other"><summary><span>{copy.others}<small>{others.length}</small></span><ChevronDown size={15} aria-hidden="true" /></summary><div>{others.map((limit) => <QuotaLimit key={limit.id} limit={limit} lang={lang} copy={copy} now={now} compact />)}</div></details>}
      <div className="cx-quota-footnote">{checked && <span><CheckCircle2 size={11} aria-hidden="true" />{copy.checked} {checked}</span>}<p>{copy.snapshotHint}</p></div>
    </>}
  </ModalShell>;
}
