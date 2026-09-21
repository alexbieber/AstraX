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
  const zh = lang === "zh";
  const tr = (cn: string, en: string) => zh ? cn : en;
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
  const validation = changedElsewhere ? tr("设置已在别处更新，请先撤销修改，再重新保存。", "Settings changed elsewhere. Discard your changes before saving again.")
    : parsed && !parsed.settings ? tr("请检查标红的输入项。", "Check the highlighted fields.")
    : draft?.takeoverEnabled && !draft.routerEnabled ? tr("请先开启路由总开关。", "Enable the routing service first.")
    : draft?.takeoverEnabled && !status?.primary?.eligible ? status?.primary?.reason || tr("请先选择一个可用的供应商或官方账号。", "Select an available provider or official account first.")
    : enablingAuto && (!draft?.routerEnabled || !draft.takeoverEnabled) ? tr("请先开启本地路由和 Codex 路由。", "Enable the local router and Codex routing first.")
    : enablingAuto && !selected.length && (!status?.primary?.eligible || status.primary.official) ? tr("请添加至少一个 API 供应商作为 P1。", "Add an API provider as P1 first.")
    : invalidQueue ? tr("队列中有不可用的供应商，请移除或修复后保存。", "Remove or fix unavailable providers in the queue before saving.") : "";
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
        setNotice(tr("路由设置已保存。", "Routing settings saved."));
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
        setNotice(tr("已清除该供应商的故障状态，可重新尝试。", "Provider health reset. It can be tried again."));
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
    if (!status?.running || !health) return <span className="cx-failover-health cx-failover-health--idle">{tr("未使用", "Not used")}</span>;
    const label = health.state === "open" ? `${tr("暂时跳过", "Unavailable")} · ${Math.ceil(health.cooldownSeconds)}s`
      : health.state === "half_open" ? tr("试探恢复", "Recovering") : tr("正常", "Healthy");
    return <span className={`cx-failover-health cx-failover-health--${health.state}`} title={tr(`连续失败 ${health.consecutiveFailures} 次 · ${health.failedRequests}/${health.totalRequests} 次失败`, `${health.consecutiveFailures} consecutive failures · ${health.failedRequests}/${health.totalRequests} failed`)}><i />{label}</span>;
  };
  const primary = status?.primary;
  const recent = status?.runtime.lastProviderId === primary?.id ? primary : providers.find((provider) => provider.id === status?.runtime.lastProviderId);
  const uptime = status ? `${Math.floor(status.runtime.uptimeSeconds / 3600)}h ${Math.floor(status.runtime.uptimeSeconds % 3600 / 60)}m` : "—";
  const fieldError = (key: keyof NonNullable<typeof parsed>["errors"]) => parsed?.errors[key];

  return <section className="cx-failover" aria-busy={loading || controlsBusy}>
    <header className="cx-failover-heading"><div><h3>{tr("路由与故障转移", "Routing & failover")}</h3><p>{tr("管理 Codex 的连接方式，让请求在供应商故障时继续完成。", "Manage Codex routing and keep requests moving when a provider is unavailable.")}</p></div><IconButton size="sm" label={tr("刷新运行状态", "Refresh routing status")} icon={<RefreshCw size={17} className={loading ? "cx-page-spin" : ""} />} disabled={controlsBusy || loading} onClick={() => setRefresh((value) => value + 1)} /></header>
    {warning && <div className="cx-failover-message cx-failover-message--warning" role="status"><AlertCircle size={18} /><div>{status && <strong>{tr("状态更新失败，仍显示上次结果。", "Unable to update status. Showing the last result.")}</strong>}<span>{warning}</span></div>{!status && <Button size="sm" variant="secondary" onClick={() => setRefresh((value) => value + 1)}>{tr("重试", "Retry")}</Button>}</div>}
    {!status || !draft ? (loading ? <div className="cx-failover-loading"><Loader2 size={20} className="cx-page-spin" />{tr("正在读取路由设置…", "Loading routing settings…")}</div> : null) : <>
      <section className="cx-failover-card">
        <div className="cx-failover-card-heading"><h4><Server size={18} />{tr("本地路由", "Local routing")}</h4><span className={`cx-failover-state${status.running ? " cx-failover-state--running" : ""}`}><i />{status.running ? tr("运行中", "Running") : tr("已停止", "Stopped")}</span></div>
        <SwitchRow id={`${id}-router`} icon={<Power size={22} />} title={tr("路由总开关", "Routing service")} hint={tr("启动本机路由服务；更改将在保存后生效。", "Run the local routing service. Changes take effect when saved.")} checked={draft.routerEnabled} disabled={controlsBusy} onChange={(checked) => edit({ routerEnabled: checked, ...(!checked ? { takeoverEnabled: false } : {}) })} />
        <div className="cx-failover-address-grid">
          <label htmlFor={`${id}-address`}>{tr("监听地址", "Listen address")}<input id={`${id}-address`} value={draft.listenAddress} disabled={controlsBusy || status.running} aria-invalid={Boolean(fieldError("listenAddress"))} onChange={(event) => edit({ listenAddress: event.target.value })} spellCheck={false} /><small className={fieldError("listenAddress") ? "cx-failover-field-error" : ""}>{fieldError("listenAddress") ? tr("请输入有效 IPv4、IPv6 或 localhost。", "Enter a valid IPv4, IPv6 or localhost.") : "IPv4 / IPv6 / localhost"}</small></label>
          <label htmlFor={`${id}-port`}>{tr("监听端口", "Listen port")}<input id={`${id}-port`} type="number" min={1024} max={65535} value={draft.listenPort} disabled={controlsBusy || status.running} aria-invalid={Boolean(fieldError("listenPort"))} onChange={(event) => edit({ listenPort: event.target.value })} /><small className={fieldError("listenPort") ? "cx-failover-field-error" : ""}>{fieldError("listenPort") ? tr("有效范围：1024–65535", "Valid range: 1024–65535") : status.running ? tr("停止并保存后可修改地址和端口。", "Stop and save before changing the address or port.") : "1024–65535"}</small></label>
        </div>
        <SwitchRow id={`${id}-takeover`} icon={<ShieldCheck size={22} />} title={tr("为 Codex 启用路由", "Route Codex requests")} hint={tr("让 Codex 请求经过本地路由；关闭后恢复直接连接。", "Send Codex requests through this router. Turn off to restore direct access.")} checked={draft.takeoverEnabled} disabled={controlsBusy || !draft.takeoverEnabled && (!draft.routerEnabled || !primary?.eligible)} onChange={(checked) => edit({ takeoverEnabled: checked })} />
        {(!primary?.eligible || !draft.routerEnabled) && <p className="cx-failover-inline-hint">{!draft.routerEnabled ? tr("先开启路由总开关，再选择是否接管 Codex。", "Enable the routing service before routing Codex requests.") : primary?.reason || tr("请先在供应商页面选择一个供应商或已登录的官方账号。", "Select a provider or a signed-in official account first.")}</p>}
        <div className="cx-failover-primary"><div className="cx-failover-provider-copy"><span className="cx-failover-label">{tr("当前供应商", "Current provider")}</span><strong>{primary?.providerName || tr("尚未选择", "None selected")}</strong><small>{primary?.baseUrl || primary?.model || "—"}</small></div>{primary?.official && <span className="cx-failover-state">{tr("官方登录", "Official account")}</span>}<span className={`cx-failover-state${status.takeoverActive ? " cx-failover-state--running" : ""}`}>{status.takeoverActive ? tr("已接管", "Routed") : tr("直接连接", "Direct access")}</span></div>
        {status.running && status.address && <div className="cx-failover-service-address"><span>{tr("当前路由地址", "Active route")}</span><code>{status.address}</code></div>}
      </section>

      <section className="cx-failover-card">
        <div className="cx-failover-card-heading"><h4><Shuffle size={18} />{tr("自动故障转移", "Automatic failover")}</h4><span className={`cx-failover-state${status.autoFailoverActive ? " cx-failover-state--running" : ""}`}>{status.autoFailoverActive ? tr("已启用", "Active") : tr("未运行", "Inactive")}</span></div>
        <SwitchRow id={`${id}-auto`} icon={<Shuffle size={22} />} title={tr("自动切换供应商", "Switch providers automatically")} hint={tr("启用并保存后会切到 P1；每次请求按 P1 → P2 → P3 的顺序尝试。", "Enabling and saving switches to P1. Each request tries P1 → P2 → P3 in order.")} checked={draft.autoFailoverEnabled} disabled={controlsBusy || !draft.autoFailoverEnabled && (!draft.routerEnabled || !draft.takeoverEnabled)} onChange={(checked) => edit({ autoFailoverEnabled: checked, ...(checked && !selected.length && primary?.eligible && !primary.official ? { providerIds: [primary.id] } : {}) })} />
        <div className="cx-failover-queue-heading"><div><h4>{tr("优先级队列", "Priority queue")}</h4><p>{tr("可以提前准备队列。请求保留 Codex 中选择的模型，请确保供应商支持它。", "Prepare your queue at any time. Requests keep the model selected in Codex; providers must support it.")}</p></div><span>{selected.length}/64</span></div>
        {!selected.length ? <div className="cx-failover-empty"><strong>{tr("还没有队列供应商", "Your queue is empty")}</strong><span>{tr("从下方添加供应商，第一位就是 P1；一个供应商也可以启用。", "Add providers below. The first is P1; a one-provider queue is also supported.")}</span></div> : <ol className="cx-failover-queue">{selected.map((providerId, index) => {
          const provider = providers.find((item) => item.id === providerId);
          const health = healthFor(providerId);
          return <li key={providerId} className={`${index === 0 ? "cx-failover-provider--first" : ""}${!provider?.eligible ? " cx-failover-provider--invalid" : ""}`}><span className="cx-failover-order">P{index + 1}</span><div className="cx-failover-provider-copy"><strong>{provider?.providerName || tr("供应商已删除", "Provider deleted")}</strong><small title={provider?.baseUrl || undefined}>{!provider?.eligible ? provider?.reason || tr("请移除此项或检查供应商配置。", "Remove this item or fix its configuration.") : provider.baseUrl || provider.model}</small></div>{healthBadge(providerId)}<div className="cx-failover-row-actions">{health && health.state !== "closed" && <IconButton size="sm" label={`${tr("重置健康状态", "Reset health")} ${provider?.providerName || ""}`} icon={<RotateCcw size={15} className={resetting === providerId ? "cx-page-spin" : ""} />} disabled={controlsBusy} onClick={() => void resetHealth(providerId)} />}<IconButton size="sm" label={`${tr("上移", "Move up")} ${provider?.providerName || ""}`} icon={<ArrowUp size={15} />} disabled={controlsBusy || index === 0} onClick={() => reorder(index, -1)} /><IconButton size="sm" label={`${tr("下移", "Move down")} ${provider?.providerName || ""}`} icon={<ArrowDown size={15} />} disabled={controlsBusy || index === selected.length - 1} onClick={() => reorder(index, 1)} /><IconButton size="sm" label={`${tr("移除", "Remove")} ${provider?.providerName || ""}`} variant="ghost" icon={<Trash2 size={15} />} disabled={controlsBusy} onClick={() => edit({ providerIds: selected.filter((item) => item !== providerId) })} /></div></li>;
        })}</ol>}
        <div className="cx-failover-candidates"><h4>{tr("添加供应商", "Add providers")}</h4>{candidates.length ? <div className="cx-failover-candidate-list">{candidates.map((provider) => <button type="button" className="cx-failover-candidate" key={provider.id} disabled={controlsBusy || !provider.eligible || selected.length >= 64} title={provider.reason || provider.baseUrl || undefined} onClick={() => edit({ providerIds: [...selected, provider.id] })}><div><strong>{provider.providerName}</strong><small>{provider.eligible ? provider.baseUrl || provider.model : provider.reason}</small></div><Plus size={17} /><span className="cx-failover-sr-only">{tr("添加", "Add")}</span></button>)}</div> : <p>{tr("所有可选供应商已在队列中；也可以到供应商页面添加更多。", "All available providers are already queued. Add more on the Providers page.")}</p>}</div>
        {primary?.official && <p className="cx-failover-inline-hint">{tr("官方账号仅使用当前登录，不参与自动切换。队列只包含 API 供应商。", "Official accounts use the current login only. Only API providers participate in failover.")}</p>}
      </section>

      <section className="cx-failover-card cx-failover-tuning"><div className="cx-failover-card-heading"><h4><Clock3 size={18} />{tr("重试、超时与恢复", "Retries, timeouts & recovery")}</h4><span className="cx-failover-section-note">{tr("自动故障转移时生效", "Applied during automatic failover")}</span></div>{(["retry", "timeout", "recovery"] as const).map((group) => <div className={`cx-failover-field-group cx-failover-field-group--${group}`} key={group}><h5>{group === "retry" ? tr("重试策略", "Retry policy") : group === "timeout" ? tr("超时时间", "Timeouts") : tr("故障恢复", "Recovery policy")}</h5><div className="cx-failover-fields">{routingFields.filter((field) => field.group === group).map((field) => <label key={field.key} htmlFor={`${id}-${field.key}`}>{zh ? field.zh : field.en}<div className="cx-failover-field-input"><input id={`${id}-${field.key}`} type="number" min={field.min} max={field.max} step={field.key === "circuitErrorRateThreshold" ? "any" : 1} value={draft[field.key]} disabled={controlsBusy} aria-invalid={Boolean(fieldError(field.key))} aria-describedby={`${id}-${field.key}-hint`} onChange={(event) => edit({ [field.key]: event.target.value })} /><span>{field.min}–{field.max}</span></div><small id={`${id}-${field.key}-hint`} className={fieldError(field.key) ? "cx-failover-field-error" : ""}>{fieldError(field.key) ? tr(`请输入 ${field.min}–${field.max} 范围内的${field.key === "circuitErrorRateThreshold" ? "数值" : "整数"}。`, `Enter ${field.min}–${field.max}${field.key === "circuitErrorRateThreshold" ? "." : " (whole numbers)."}`) : zh ? field.hintZh : field.hintEn}</small></label>)}</div></div>)}</section>

      <section className="cx-failover-card cx-failover-runtime"><div className="cx-failover-card-heading"><h4><Activity size={18} />{tr("运行状态", "Runtime status")}</h4><span className="cx-failover-section-note">{tr("运行时间", "Uptime")} {uptime}</span></div><dl><div><dt>{tr("处理请求", "Requests")}</dt><dd>{status.runtime.requestCount.toLocaleString()}</dd></div><div><dt>{tr("成功 / 失败", "Succeeded / failed")}</dt><dd>{status.runtime.successCount.toLocaleString()} <span>/ {status.runtime.failureCount.toLocaleString()}</span></dd></div><div><dt>{tr("自动切换", "Failovers")}</dt><dd>{status.runtime.failoverCount.toLocaleString()}</dd></div><div><dt>{tr("正在处理", "In progress")}</dt><dd>{status.runtime.inFlight.toLocaleString()}</dd></div></dl><div className="cx-failover-runtime-current"><span>{tr("最近使用", "Last used")}</span><strong>{recent?.providerName || tr("等待请求", "Waiting for requests")}</strong>{status.runtime.lastRequestAt && <time>{new Date(status.runtime.lastRequestAt).toLocaleTimeString(lang === "zh" ? "zh-CN" : "en-US", { hour: "2-digit", minute: "2-digit" })}</time>}</div>{status.message && <p className="cx-failover-runtime-note">{status.message}</p>}{status.runtime.lastError && <p className="cx-failover-runtime-note"><AlertCircle size={15} />{status.runtime.lastError}</p>}</section>
      <p className="cx-failover-bottom-note">{tr("回复已经开始后不会重复发送。关闭 AstraX 窗口仍会在后台运行；完全退出时恢复直接连接。", "A reply that has already started is never replayed. Closing this window keeps routing active; quitting AstraX restores direct access.")}</p>
      {saveError && <div className="cx-failover-message cx-failover-message--warning" role="alert"><AlertCircle size={18} /><span>{saveError}</span></div>}
      {notice && <div className="cx-failover-message cx-failover-message--success" role="status"><CheckCircle2 size={18} /><span>{notice}</span></div>}
      <footer className="cx-failover-save"><div aria-live="polite" className={validation ? "cx-failover-field-error" : ""}>{validation || (dirty ? tr("有未保存的修改", "Unsaved changes") : tr("更改将在保存后生效", "Changes take effect when saved"))}</div><div><Button size="sm" variant="secondary" disabled={!dirty || controlsBusy} onClick={reset} icon={<RotateCcw size={15} />}>{tr("撤销修改", "Discard changes")}</Button><Button size="sm" disabled={!dirty || controlsBusy || Boolean(validation)} icon={saving ? <Loader2 size={15} className="cx-page-spin" /> : undefined} onClick={() => void save()}>{saving ? tr("保存中…", "Saving…") : tr("保存设置", "Save settings")}</Button></div></footer>
    </>}
  </section>;
}
