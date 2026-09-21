import { useEffect, useId, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Activity, AlertCircle, ArrowDown, ArrowUp, CheckCircle2, Clock3, Loader2, Plus, Power, RefreshCw, RotateCcw, Server, ShieldCheck, Shuffle, Trash2 } from "lucide-react";
import { Button, IconButton } from "../components/ui";
import { parseRoutingDraft, routingDraft, routingFields, sameRoutingDraft, type RoutingDraft, type RoutingSettings } from "../routingSettings";
import "../styles/provider-failover.css";

type Language = "zh" | "en";
type Provider = { id: string; providerName: string; model: string; models: string[]; baseUrl?: string | null; eligible: boolean; reason: string | null; official: boolean };
type Health = { id: string; state: "closed" | "open" | "half_open"; cooldownSeconds: number; lastStatus: number | null; consecutiveFailures: number; consecutiveSuccesses: number; totalRequests: number; failedRequests: number };
export type FailoverStatus = {
  settings: RoutingSettings; running: boolean; takeoverActive: boolean; autoFailoverActive: boolean; address: string | null;
  primary: Provider | null; providers: Provider[];
  runtime: { requestCount: number; failoverCount: number; inFlight: number; successCount: number; failureCount: number; uptimeSeconds: number; lastRequestAt: string | null; lastProviderId: string | null; lastError: string | null; providers: Health[] };
  message: string | null;
};
type PageState = { dir: string; status: FailoverStatus; draft: RoutingDraft; baseline: RoutingDraft };
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);

function SwitchRow({ id, icon, title, hint, checked, disabled, onChange }: {
  id: string; icon: React.ReactNode; title: string; hint: string; checked: boolean; disabled?: boolean; onChange: (checked: boolean) => void;
}) {
  return <div className="cx-failover-toggle-row"><span className="cx-failover-symbol">{icon}</span><div className="cx-failover-toggle-copy"><label id={`${id}-label`} htmlFor={id}>{title}</label><p id={`${id}-hint`}>{hint}</p></div><button id={id} className="cx-failover-switch" type="button" role="switch" aria-checked={checked} aria-labelledby={`${id}-label`} aria-describedby={`${id}-hint`} disabled={disabled} onClick={() => onChange(!checked)}><span /></button></div>;
}

export function ProviderFailoverPage({ lang, configDir, active = true, onChange }: {
  lang: Language; configDir: string; active?: boolean; onChange?: () => void | Promise<void>;
}) {
  const id = useId();
  const [page, setPage] = useState<PageState | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [resetting, setResetting] = useState<string | null>(null);
  const [warning, setWarning] = useState("");
  const [saveError, setSaveError] = useState("");
  const [notice, setNotice] = useState("");
  const [refresh, setRefresh] = useState(0);
  const generation = useRef(0);
  const busy = useRef(false);
  const mounted = useRef(true);
  const current = useRef({ active, configDir });
  const pageRef = useRef(page);
  current.current = { active, configDir };
  pageRef.current = page;

  const acceptStatus = (status: FailoverStatus, preserveDraft: boolean) => {
    setPage((previous) => {
      const saved = routingDraft(status.settings);
      const keep = preserveDraft && previous?.dir === configDir && !sameRoutingDraft(previous.draft, previous.baseline)
        && !sameRoutingDraft(previous.draft, saved);
      return { dir: configDir, status, draft: keep ? previous.draft : saved, baseline: keep ? previous.baseline : routingDraft(status.settings) };
    });
  };
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => { setNotice(""); setSaveError(""); setWarning(""); }, [configDir]);
  useEffect(() => {
    const version = ++generation.current;
    if (!active) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const valid = () => mounted.current && generation.current === version && current.current.active && current.current.configDir === configDir;
    const load = async () => {
      if (!valid()) return;
      if (busy.current || document.hidden) { timer = setTimeout(() => void load(), 5000); return; }
      setLoading(pageRef.current?.dir !== configDir);
      try {
        const status = await invoke<FailoverStatus>("get_provider_failover", { configDir: configDir || null });
        if (!valid()) return;
        acceptStatus(status, true);
        setWarning("");
      } catch (error) {
        if (valid()) setWarning(errorText(error));
      } finally {
        if (valid()) { setLoading(false); timer = setTimeout(() => void load(), 5000); }
      }
    };
    void load();
    return () => { ++generation.current; if (timer) clearTimeout(timer); };
  }, [active, configDir, refresh]);

  const view = page?.dir === configDir ? page : null;
  const status = view?.status;
  const draft = view?.draft;
  const dirty = Boolean(view && !sameRoutingDraft(view.draft, view.baseline));
  const changedElsewhere = Boolean(view && dirty && !sameRoutingDraft(routingDraft(view.status.settings), view.baseline));
  const parsed = draft ? parseRoutingDraft(draft) : null;
  const selected = draft?.providerIds ?? [];
  const providers = status?.providers.filter((provider) => !provider.official) ?? [];
  const candidates = providers.filter((provider) => !selected.includes(provider.id));
  const invalidQueue = selected.some((item) => !providers.find((provider) => provider.id === item)?.eligible);
  const enablingAuto = Boolean(draft?.autoFailoverEnabled && !status?.settings.autoFailoverEnabled);
  const validation = changedElsewhere ? "Settings changed elsewhere. Discard your changes before saving again."
    : parsed && !parsed.settings ? "Check the highlighted fields."
    : draft?.takeoverEnabled && !draft.routerEnabled ? "Enable the routing service first."
    : draft?.takeoverEnabled && !status?.primary?.eligible ? status?.primary?.reason || "Select an available provider or official account first."
    : enablingAuto && (!draft?.routerEnabled || !draft.takeoverEnabled) ? "Enable the local router and Codex routing first."
    : enablingAuto && !selected.length && (!status?.primary?.eligible || status.primary.official) ? "Add an API provider as P1 first."
    : invalidQueue ? "Remove or fix unavailable providers in the queue before saving." : "";
  const controlsBusy = saving || resetting !== null;
  const edit = (patch: Partial<RoutingDraft>) => {
    if (busy.current) return;
    setPage((previous) => previous?.dir === configDir ? { ...previous, draft: { ...previous.draft, ...patch } } : previous);
    setSaveError(""); setNotice("");
  };
  const reset = () => {
    if (!view || busy.current) return;
    acceptStatus(view.status, false); setNotice(""); setSaveError("");
  };
  const reorder = (index: number, offset: number) => {
    if (index + offset < 0 || index + offset >= selected.length) return;
    const providerIds = [...selected];
    [providerIds[index], providerIds[index + offset]] = [providerIds[index + offset], providerIds[index]];
    edit({ providerIds });
  };
  const save = async () => {
    if (!view || !draft || !dirty || validation || busy.current || !active || !parsed?.settings) return;
    const version = ++generation.current;
    busy.current = true; setSaving(true); setSaveError(""); setNotice("");
    try {
      const result = await invoke<FailoverStatus>("save_provider_failover", { configDir: configDir || null, settings: parsed.settings });
      if (mounted.current && current.current.configDir === configDir && generation.current === version) {
        acceptStatus(result, false); setWarning("");
        setNotice("Routing settings saved.");
      }
      if (current.current.configDir === configDir) {
        try { await onChange?.(); } catch { /* A later status read remains authoritative. */ }
      }
    } catch (error) {
      if (mounted.current && current.current.configDir === configDir && generation.current === version) setSaveError(errorText(error));
    } finally {
      busy.current = false;
      if (mounted.current) { setSaving(false); setRefresh((value) => value + 1); }
    }
  };
  const resetHealth = async (providerId: string) => {
    if (busy.current || !active) return;
    const version = ++generation.current;
    busy.current = true; setResetting(providerId); setSaveError(""); setNotice("");
    try {
      const result = await invoke<FailoverStatus>("reset_provider_failover_health", { configDir: configDir || null, providerId });
      if (mounted.current && current.current.configDir === configDir && generation.current === version) {
        acceptStatus(result, true);
        setNotice("Provider health reset. It can be tried again.");
      }
    } catch (error) {
      if (mounted.current && current.current.configDir === configDir && generation.current === version) setSaveError(errorText(error));
    } finally {
      busy.current = false;
      if (mounted.current) { setResetting(null); setRefresh((value) => value + 1); }
    }
  };
  const healthFor = (providerId: string) => status?.runtime.providers.find((provider) => provider.id === providerId);
  const healthBadge = (providerId: string) => {
    const health = healthFor(providerId);
    if (!status?.running || !health) return <span className="cx-failover-health cx-failover-health--idle">{"Not used"}</span>;
    const label = health.state === "open" ? `${"Unavailable"} · ${Math.ceil(health.cooldownSeconds)}s`
      : health.state === "half_open" ? "Recovering" : "Healthy";
    return <span className={`cx-failover-health cx-failover-health--${health.state}`} title={`${health.consecutiveFailures} consecutive failures · ${health.failedRequests}/${health.totalRequests} failed`}><i />{label}</span>;
  };
  const primary = status?.primary;
  const recent = status?.runtime.lastProviderId === primary?.id ? primary : providers.find((provider) => provider.id === status?.runtime.lastProviderId);
  const uptime = status ? `${Math.floor(status.runtime.uptimeSeconds / 3600)}h ${Math.floor(status.runtime.uptimeSeconds % 3600 / 60)}m` : "—";
  const fieldError = (key: keyof NonNullable<typeof parsed>["errors"]) => parsed?.errors[key];

  return <section className="cx-failover" aria-busy={loading || controlsBusy}>
    <header className="cx-failover-heading"><div><h3>{"Routing & failover"}</h3><p>{"Manage Codex routing and keep requests moving when a provider is unavailable."}</p></div><IconButton size="sm" label={"Refresh routing status"} icon={<RefreshCw size={17} className={loading ? "cx-page-spin" : ""} />} disabled={controlsBusy || loading} onClick={() => setRefresh((value) => value + 1)} /></header>
    {warning && <div className="cx-failover-message cx-failover-message--warning" role="status"><AlertCircle size={18} /><div>{status && <strong>{"Unable to update status. Showing the last result."}</strong>}<span>{warning}</span></div>{!status && <Button size="sm" variant="secondary" onClick={() => setRefresh((value) => value + 1)}>{"Retry"}</Button>}</div>}
    {!status || !draft ? (loading ? <div className="cx-failover-loading"><Loader2 size={20} className="cx-page-spin" />{"Loading routing settings…"}</div> : null) : <>
      <section className="cx-failover-card">
        <div className="cx-failover-card-heading"><h4><Server size={18} />{"Local routing"}</h4><span className={`cx-failover-state${status.running ? " cx-failover-state--running" : ""}`}><i />{status.running ? "Running" : "Stopped"}</span></div>
        <SwitchRow id={`${id}-router`} icon={<Power size={22} />} title={"Routing service"} hint={"Run the local routing service. Changes take effect when saved."} checked={draft.routerEnabled} disabled={controlsBusy} onChange={(checked) => edit({ routerEnabled: checked, ...(!checked ? { takeoverEnabled: false } : {}) })} />
        <div className="cx-failover-address-grid">
          <label htmlFor={`${id}-address`}>{"Listen address"}<input id={`${id}-address`} value={draft.listenAddress} disabled={controlsBusy || status.running} aria-invalid={Boolean(fieldError("listenAddress"))} onChange={(event) => edit({ listenAddress: event.target.value })} spellCheck={false} /><small className={fieldError("listenAddress") ? "cx-failover-field-error" : ""}>{fieldError("listenAddress") ? "Enter a valid IPv4, IPv6 or localhost." : "IPv4 / IPv6 / localhost"}</small></label>
          <label htmlFor={`${id}-port`}>{"Listen port"}<input id={`${id}-port`} type="number" min={1024} max={65535} value={draft.listenPort} disabled={controlsBusy || status.running} aria-invalid={Boolean(fieldError("listenPort"))} onChange={(event) => edit({ listenPort: event.target.value })} /><small className={fieldError("listenPort") ? "cx-failover-field-error" : ""}>{fieldError("listenPort") ? "Valid range: 1024–65535" : status.running ? "Stop and save before changing the address or port." : "1024–65535"}</small></label>
        </div>
        <SwitchRow id={`${id}-takeover`} icon={<ShieldCheck size={22} />} title={"Route Codex requests"} hint={"Send Codex requests through this router. Turn off to restore direct access."} checked={draft.takeoverEnabled} disabled={controlsBusy || !draft.takeoverEnabled && (!draft.routerEnabled || !primary?.eligible)} onChange={(checked) => edit({ takeoverEnabled: checked })} />
        {(!primary?.eligible || !draft.routerEnabled) && <p className="cx-failover-inline-hint">{!draft.routerEnabled ? "Enable the routing service before routing Codex requests." : primary?.reason || "Select a provider or a signed-in official account first."}</p>}
        <div className="cx-failover-primary"><div className="cx-failover-provider-copy"><span className="cx-failover-label">{"Current provider"}</span><strong>{primary?.providerName || "None selected"}</strong><small>{primary?.baseUrl || primary?.model || "—"}</small></div>{primary?.official && <span className="cx-failover-state">{"Official account"}</span>}<span className={`cx-failover-state${status.takeoverActive ? " cx-failover-state--running" : ""}`}>{status.takeoverActive ? "Routed" : "Direct access"}</span></div>
        {status.running && status.address && <div className="cx-failover-service-address"><span>{"Active route"}</span><code>{status.address}</code></div>}
      </section>

      <section className="cx-failover-card">
        <div className="cx-failover-card-heading"><h4><Shuffle size={18} />{"Automatic failover"}</h4><span className={`cx-failover-state${status.autoFailoverActive ? " cx-failover-state--running" : ""}`}>{status.autoFailoverActive ? "Active" : "Inactive"}</span></div>
        <SwitchRow id={`${id}-auto`} icon={<Shuffle size={22} />} title={"Switch providers automatically"} hint={"Enabling and saving switches to P1. Each request tries P1 → P2 → P3 in order."} checked={draft.autoFailoverEnabled} disabled={controlsBusy || !draft.autoFailoverEnabled && (!draft.routerEnabled || !draft.takeoverEnabled)} onChange={(checked) => edit({ autoFailoverEnabled: checked, ...(checked && !selected.length && primary?.eligible && !primary.official ? { providerIds: [primary.id] } : {}) })} />
        <div className="cx-failover-queue-heading"><div><h4>{"Priority queue"}</h4><p>{"Prepare your queue at any time. Requests keep the model selected in Codex; providers must support it."}</p></div><span>{selected.length}/64</span></div>
        {!selected.length ? <div className="cx-failover-empty"><strong>{"Your queue is empty"}</strong><span>{"Add providers below. The first is P1; a one-provider queue is also supported."}</span></div> : <ol className="cx-failover-queue">{selected.map((providerId, index) => {
          const provider = providers.find((item) => item.id === providerId);
          const health = healthFor(providerId);
          return <li key={providerId} className={`${index === 0 ? "cx-failover-provider--first" : ""}${!provider?.eligible ? " cx-failover-provider--invalid" : ""}`}><span className="cx-failover-order">P{index + 1}</span><div className="cx-failover-provider-copy"><strong>{provider?.providerName || "Provider deleted"}</strong><small title={provider?.baseUrl || undefined}>{!provider?.eligible ? provider?.reason || "Remove this item or fix its configuration." : provider.baseUrl || provider.model}</small></div>{healthBadge(providerId)}<div className="cx-failover-row-actions">{health && health.state !== "closed" && <IconButton size="sm" label={`${"Reset health"} ${provider?.providerName || ""}`} icon={<RotateCcw size={15} className={resetting === providerId ? "cx-page-spin" : ""} />} disabled={controlsBusy} onClick={() => void resetHealth(providerId)} />}<IconButton size="sm" label={`${"Move up"} ${provider?.providerName || ""}`} icon={<ArrowUp size={15} />} disabled={controlsBusy || index === 0} onClick={() => reorder(index, -1)} /><IconButton size="sm" label={`${"Move down"} ${provider?.providerName || ""}`} icon={<ArrowDown size={15} />} disabled={controlsBusy || index === selected.length - 1} onClick={() => reorder(index, 1)} /><IconButton size="sm" label={`${"Remove"} ${provider?.providerName || ""}`} variant="ghost" icon={<Trash2 size={15} />} disabled={controlsBusy} onClick={() => edit({ providerIds: selected.filter((item) => item !== providerId) })} /></div></li>;
        })}</ol>}
        <div className="cx-failover-candidates"><h4>{"Add providers"}</h4>{candidates.length ? <div className="cx-failover-candidate-list">{candidates.map((provider) => <button type="button" className="cx-failover-candidate" key={provider.id} disabled={controlsBusy || !provider.eligible || selected.length >= 64} title={provider.reason || provider.baseUrl || undefined} onClick={() => edit({ providerIds: [...selected, provider.id] })}><div><strong>{provider.providerName}</strong><small>{provider.eligible ? provider.baseUrl || provider.model : provider.reason}</small></div><Plus size={17} /><span className="cx-failover-sr-only">{"Add"}</span></button>)}</div> : <p>{"All available providers are already queued. Add more on the Providers page."}</p>}</div>
        {primary?.official && <p className="cx-failover-inline-hint">{"Official accounts use the current login only. Only API providers participate in failover."}</p>}
      </section>

      <section className="cx-failover-card cx-failover-tuning"><div className="cx-failover-card-heading"><h4><Clock3 size={18} />{"Retries, timeouts & recovery"}</h4><span className="cx-failover-section-note">{"Applied during automatic failover"}</span></div>{(["retry", "timeout", "recovery"] as const).map((group) => <div className={`cx-failover-field-group cx-failover-field-group--${group}`} key={group}><h5>{group === "retry" ? "Retry policy" : group === "timeout" ? "Timeouts" : "Recovery policy"}</h5><div className="cx-failover-fields">{routingFields.filter((field) => field.group === group).map((field) => <label key={field.key} htmlFor={`${id}-${field.key}`}>{field.en}<div className="cx-failover-field-input"><input id={`${id}-${field.key}`} type="number" min={field.min} max={field.max} step={field.key === "circuitErrorRateThreshold" ? "any" : 1} value={draft[field.key]} disabled={controlsBusy} aria-invalid={Boolean(fieldError(field.key))} aria-describedby={`${id}-${field.key}-hint`} onChange={(event) => edit({ [field.key]: event.target.value })} /><span>{field.min}–{field.max}</span></div><small id={`${id}-${field.key}-hint`} className={fieldError(field.key) ? "cx-failover-field-error" : ""}>{fieldError(field.key) ? `Enter ${field.min}–${field.max}${field.key === "circuitErrorRateThreshold" ? "." : " (whole numbers)."}` : field.hintEn}</small></label>)}</div></div>)}</section>

      <section className="cx-failover-card cx-failover-runtime"><div className="cx-failover-card-heading"><h4><Activity size={18} />{"Runtime status"}</h4><span className="cx-failover-section-note">{"Uptime"} {uptime}</span></div><dl><div><dt>{"Requests"}</dt><dd>{status.runtime.requestCount.toLocaleString()}</dd></div><div><dt>{"Succeeded / failed"}</dt><dd>{status.runtime.successCount.toLocaleString()} <span>/ {status.runtime.failureCount.toLocaleString()}</span></dd></div><div><dt>{"Failovers"}</dt><dd>{status.runtime.failoverCount.toLocaleString()}</dd></div><div><dt>{"In progress"}</dt><dd>{status.runtime.inFlight.toLocaleString()}</dd></div></dl><div className="cx-failover-runtime-current"><span>{"Last used"}</span><strong>{recent?.providerName || "Waiting for requests"}</strong>{status.runtime.lastRequestAt && <time>{new Date(status.runtime.lastRequestAt).toLocaleTimeString("en-US", { hour: "2-digit", minute: "2-digit" })}</time>}</div>{status.message && <p className="cx-failover-runtime-note">{status.message}</p>}{status.runtime.lastError && <p className="cx-failover-runtime-note"><AlertCircle size={15} />{status.runtime.lastError}</p>}</section>
      <p className="cx-failover-bottom-note">{"A reply that has already started is never replayed. Closing this window keeps routing active; quitting AstraX restores direct access."}</p>
      {saveError && <div className="cx-failover-message cx-failover-message--warning" role="alert"><AlertCircle size={18} /><span>{saveError}</span></div>}
      {notice && <div className="cx-failover-message cx-failover-message--success" role="status"><CheckCircle2 size={18} /><span>{notice}</span></div>}
      <footer className="cx-failover-save"><div aria-live="polite" className={validation ? "cx-failover-field-error" : ""}>{validation || (dirty ? "Unsaved changes" : "Changes take effect when saved")}</div><div><Button size="sm" variant="secondary" disabled={!dirty || controlsBusy} onClick={reset} icon={<RotateCcw size={15} />}>{"Discard changes"}</Button><Button size="sm" disabled={!dirty || controlsBusy || Boolean(validation)} icon={saving ? <Loader2 size={15} className="cx-page-spin" /> : undefined} onClick={() => void save()}>{saving ? "Saving…" : "Save settings"}</Button></div></footer>
    </>}
  </section>;
}
