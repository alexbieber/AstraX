import { useCallback, useEffect, useId, useMemo, useReducer, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Activity, AlertCircle, ArrowDownLeft, ArrowUpRight, BarChart3,
  CalendarDays, Check, Database, Loader2, MessageSquare, RefreshCw,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { UsageDay, UsageModel, UsageRange, UsageStatistics } from "../usageTypes";
import { initialUsageLoadState, sameUsageQuery, usageLoadReducer } from "../usageStatisticsState";
import "../styles/usage-statistics.css";

type Language = "zh" | "en";

export type UsageStatisticsPageProps = {
  lang: Language;
  configDir: string;
  active?: boolean;
};

export function formatUsageTokens(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "0";
  const units: [number, string][] = [[1_000_000_000, "B"], [1_000_000, "M"], [1_000, "K"]];
  for (const [divisor, unit] of units) {
    if (value >= divisor) {
      const scaled = value / divisor;
      return `${Number(scaled.toFixed(scaled < 10 ? 2 : 1))}${unit}`;
    }
  }
  return Math.round(value).toString();
}

const exact = (value: number, lang: Language) => value.toLocaleString(lang === "zh" ? "zh-CN" : "en-US");
const ratio = (part: number, total: number) => total > 0 ? Math.min(100, Math.max(0, part / total * 100)) : 0;
const percentage = (value: number) => `${Number(value.toFixed(1))}%`;

function modelLabel(model: string, lang: Language): string {
  if (model === "unknown") return lang === "zh" ? "未知模型" : "Unknown model";
  if (model === "multiple") return lang === "zh" ? "多个模型" : "Multiple models";
  return model;
}

function sessionTitleLabel(title: string, lang: Language): string {
  return title && title !== "未命名会话" ? title : lang === "zh" ? "未命名会话" : "Untitled session";
}

function dateLabel(date: string, lang: Language, includeYear = false): string {
  const value = new Date(date.length === 10 ? `${date}T12:00:00` : date);
  if (Number.isNaN(value.getTime())) return date;
  return value.toLocaleDateString(lang === "zh" ? "zh-CN" : "en-US", {
    ...(includeYear ? { year: "numeric" as const } : {}), month: "short", day: "numeric",
  });
}

function timestampLabel(date: string, lang: Language): string {
  const value = new Date(date);
  return Number.isNaN(value.getTime()) ? date : value.toLocaleString(lang === "zh" ? "zh-CN" : "en-US", {
    month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false,
  });
}

function copyFor(lang: Language) {
  return lang === "zh" ? {
    title: "Token 用量统计",
    description: "查看 Codex 的 Token 使用情况，了解用量趋势与模型分布。",
    range: "统计范围", ranges: { today: "今天", "7d": "近 7 天", "30d": "近 30 天", all: "全部" },
    model: "模型", allModels: "全部模型", refresh: "刷新用量", refreshing: "更新中", retry: "重试",
    total: "总 Token 用量", totalHint: "输入 + 输出", input: "输入 Token", output: "输出 Token",
    freshInput: "未缓存输入", cache: "缓存输入", cached: "已缓存", cacheRate: "缓存命中率",
    inputHint: "包含缓存输入", outputHint: "包含推理 Token", reasoning: "推理 Token",
    sessions: "活跃会话", sessionsHint: "主会话数量，含子代理产生的用量", tokens: "Token",
    trend: "用量趋势", trendHint: "按日查看输入、缓存和输出的构成", peak: "最高用量日",
    legendLabel: "Token 构成", chartLabel: "每日 Token 用量；左右方向键切换日期",
    breakdown: "每日明细", date: "日期", totalColumn: "合计", modelTitle: "模型分布",
    modelHint: "按 Token 用量排序", modelCount: "个模型", modelFilter: "筛选此模型",
    recent: "最近会话", recentHint: "展示最近 10 个有用量的主会话；子代理用量合并到所属主会话，总量包含全部符合条件的记录。",
    session: "会话", lastActive: "最近活动", noData: "这里将记录你的 Codex 用量",
    noDataDescription: "当前范围内还没有可统计的会话记录。使用 Codex 后点击刷新，或试试更大的日期范围。",
    showAll: "查看全部时间", loading: "正在整理本地会话用量…",
    error: "暂时无法读取用量", stale: "刷新失败，下面保留上次读取的结果。",
    updatingResults: "正在更新用量，保留当前结果。", showingResults: "当前显示",
    loadingResults: "正在加载", previousResults: "下方暂时显示", failedResults: "未能加载",
    partial: "部分记录未计入", partialHint: "部分本地记录不完整，当前结果仅包含可读取的用量。",
    source: "本地会话记录", updated: "更新于", notes: "缓存输入已包含在输入 Token 中，推理 Token 已包含在输出 Token 中。历史记录缺失的用量无法补算。",
    scanned: "已检查", files: "份记录", filtered: "当前模型", activity: "使用概览",
    noModels: "暂无模型用量", zeroDay: "当天无用量",
  } : {
    title: "Token usage statistics",
    description: "Explore your Codex usage, daily activity, and model distribution.",
    range: "Date range", ranges: { today: "Today", "7d": "7 days", "30d": "30 days", all: "All time" },
    model: "Model", allModels: "All models", refresh: "Refresh usage", refreshing: "Updating", retry: "Retry",
    total: "Total token usage", totalHint: "Input + output", input: "Input tokens", output: "Output tokens",
    freshInput: "Uncached input", cache: "Cached input", cached: "cached", cacheRate: "Cache hit rate",
    inputHint: "Includes cached input", outputHint: "Includes reasoning tokens", reasoning: "Reasoning tokens",
    sessions: "Active sessions", sessionsHint: "Main conversations, including subagent usage", tokens: "tokens",
    trend: "Usage over time", trendHint: "Daily input, cache, and output breakdown", peak: "Peak day",
    legendLabel: "Token breakdown", chartLabel: "Daily token usage; use left and right arrows to explore dates",
    breakdown: "Daily details", date: "Date", totalColumn: "Total", modelTitle: "Model distribution",
    modelHint: "Ranked by token usage", modelCount: "models", modelFilter: "Filter by this model",
    recent: "Recent sessions", recentHint: "Showing the 10 most recent main conversations. Subagent usage is included in its parent conversation; totals include all matching records.",
    session: "Session", lastActive: "Last active", noData: "Your Codex usage will appear here",
    noDataDescription: "No usage records in this range yet. Refresh after using Codex, or try a broader date range.",
    showAll: "View all time", loading: "Reading local session usage…",
    error: "Unable to load usage", stale: "Refresh failed. The last available results are shown below.",
    updatingResults: "Updating usage. Current results stay visible.", showingResults: "Showing",
    loadingResults: "Loading", previousResults: "Results below", failedResults: "Unable to load",
    partial: "Some records were excluded", partialHint: "Some local records are incomplete. These results include readable usage only.",
    source: "Local session records", updated: "Updated", notes: "Cached input is included in input tokens; reasoning is included in output tokens. Usage cannot be recovered from missing historical records.",
    scanned: "Checked", files: "records", filtered: "Selected model", activity: "Usage overview",
    noModels: "No model usage yet", zeroDay: "No usage this day",
  };
}

type UsageCopy = ReturnType<typeof copyFor>;

function TokenValue({ value, lang, className = "" }: { value: number; lang: Language; className?: string }) {
  return <span className={`cx-usage-number ${className}`} title={exact(value, lang)} aria-label={exact(value, lang)}>{formatUsageTokens(value)}</span>;
}

function Metric({ icon: Icon, label, children, hint, accent }: {
  icon: LucideIcon; label: string; children: ReactNode; hint: ReactNode; accent: string;
}) {
  return <article className={`cx-usage-metric cx-usage-metric--${accent}`}>
    <div className="cx-usage-metric-label"><Icon size={15} aria-hidden="true" /><span>{label}</span></div>
    <div className="cx-usage-metric-value">{children}</div>
    <p>{hint}</p>
  </article>;
}

function Legend({ copy }: { copy: UsageCopy }) {
  return <div className="cx-usage-legend" aria-label={copy.legendLabel}>
    <span><i className="cx-usage-swatch--input" />{copy.freshInput}</span>
    <span><i className="cx-usage-swatch--cache" />{copy.cache}</span>
    <span><i className="cx-usage-swatch--output" />{copy.output}</span>
  </div>;
}

function UsageTrend({ days, lang, copy }: { days: UsageDay[]; lang: Language; copy: UsageCopy }) {
  const chartId = useId();
  const [selectedDate, setSelectedDate] = useState<string | null>(null);
  const barsRef = useRef<(SVGGElement | null)[]>([]);
  const peakIndex = days.reduce((winner, day, index) => day.totalTokens > (days[winner]?.totalTokens ?? -1) ? index : winner, 0);
  const selectedIndex = Math.max(0, selectedDate ? days.findIndex((day) => day.date === selectedDate) : peakIndex);
  const selected = days[selectedIndex];
  const chartWidth = Math.max(600, days.length * 13 + 56);
  const chartHeight = 218;
  const baseline = 181;
  const top = 15;
  const left = 48;
  const drawableWidth = chartWidth - left - 12;
  const max = Math.max(...days.map((day) => day.inputTokens + day.outputTokens), 1);
  const ceiling = max <= 5 ? 5 : Math.ceil(max / 10 ** Math.floor(Math.log10(max))) * 10 ** Math.floor(Math.log10(max));
  const step = drawableWidth / Math.max(days.length, 1);
  const width = Math.min(38, step * 0.62);
  const spansYears = days[0]?.date.slice(0, 4) !== days[days.length - 1]?.date.slice(0, 4);
  const scale = (value: number) => value / ceiling * (baseline - top);
  const keyDown = (event: KeyboardEvent<SVGGElement>, index: number) => {
    let next = index;
    if (event.key === "Enter" || event.key === " ") next = index;
    else if (event.key === "ArrowLeft") next = Math.max(0, index - 1);
    else if (event.key === "ArrowRight") next = Math.min(days.length - 1, index + 1);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = days.length - 1;
    else return;
    event.preventDefault();
    setSelectedDate(days[next].date);
    barsRef.current[next]?.focus();
  };

  return <section className="cx-usage-card cx-usage-trend">
    <div className="cx-usage-card-heading"><div><h3>{copy.trend}</h3><p>{copy.trendHint}</p></div><BarChart3 size={18} aria-hidden="true" /></div>
    <Legend copy={copy} />
    {selected && <div className="cx-usage-day-summary" id={`${chartId}-selection`} aria-live="polite" aria-atomic="true">
      <div><strong>{dateLabel(selected.date, lang, true)}</strong>{selectedIndex === peakIndex && selected.totalTokens > 0 && <span>{copy.peak}</span>}</div>
      <div><TokenValue value={selected.totalTokens} lang={lang} /><small>{copy.tokens}</small></div>
      <p>{copy.input} {exact(selected.inputTokens, lang)} · {copy.cache} {exact(selected.cachedInputTokens, lang)} · {copy.output} {exact(selected.outputTokens, lang)}</p>
    </div>}
    <div className="cx-usage-chart-scroll">
      <svg className="cx-usage-chart" viewBox={`0 0 ${chartWidth} ${chartHeight}`} style={{ minWidth: Math.max(440, days.length * 11 + 56) }} role="group" aria-label={copy.chartLabel}>
        {[0, 0.5, 1].map((fraction) => <g key={fraction} aria-hidden="true">
          <line x1={left} x2={chartWidth - 12} y1={baseline - scale(ceiling * fraction)} y2={baseline - scale(ceiling * fraction)} className="cx-usage-gridline" />
          <text x={left - 9} y={baseline - scale(ceiling * fraction) + 3} textAnchor="end" className="cx-usage-axis">{formatUsageTokens(ceiling * fraction)}</text>
        </g>)}
        {days.map((day, index) => {
          const cached = Math.min(day.cachedInputTokens, day.inputTokens);
          const fresh = Math.max(0, day.inputTokens - cached);
          const inputHeight = scale(fresh);
          const cachedHeight = scale(cached);
          const outputHeight = scale(day.outputTokens);
          const x = left + step * index + (step - width) / 2;
          const showLabel = days.length <= 8 || index === days.length - 1 || index % Math.ceil(days.length / 7) === 0;
          const label = `${dateLabel(day.date, lang, true)}: ${copy.input} ${exact(day.inputTokens, lang)}, ${copy.cache} ${exact(cached, lang)}, ${copy.output} ${exact(day.outputTokens, lang)}, ${copy.totalColumn} ${exact(day.totalTokens, lang)}`;
          return <g key={day.date} ref={(element) => { barsRef.current[index] = element; }} className={`cx-usage-day${index === selectedIndex ? " cx-usage-day--selected" : ""}`} role="button" tabIndex={index === selectedIndex ? 0 : -1} aria-label={label} aria-pressed={index === selectedIndex} onMouseEnter={() => setSelectedDate(day.date)} onFocus={() => setSelectedDate(day.date)} onClick={() => setSelectedDate(day.date)} onKeyDown={(event) => keyDown(event, index)}>
            <title>{label}</title>
            <rect x={left + step * index} y={top - 4} width={step} height={baseline - top + 10} rx={4} className="cx-usage-chart-hit" />
            <rect x={x} y={baseline - inputHeight} width={width} height={inputHeight} className="cx-usage-swatch--input" />
            <rect x={x} y={baseline - inputHeight - cachedHeight} width={width} height={cachedHeight} className="cx-usage-swatch--cache" />
            <rect x={x} y={baseline - inputHeight - cachedHeight - outputHeight} width={width} height={outputHeight} className="cx-usage-swatch--output" />
            {day.totalTokens === 0 && <rect x={x} y={baseline - 2} width={width} height={2} rx={1} className="cx-usage-zero-bar" />}
            {showLabel && <text x={x + width / 2} y={baseline + 22} textAnchor="middle" className="cx-usage-axis" aria-hidden="true">{dateLabel(day.date, lang, spansYears)}</text>}
          </g>;
        })}
      </svg>
    </div>
    <details className="cx-usage-daily-details"><summary>{copy.breakdown}<span>{days.length}</span></summary>
      <div className="cx-usage-table-scroll"><table className="cx-usage-table"><caption className="cx-usage-sr-only">{copy.breakdown}</caption><thead><tr><th>{copy.date}</th><th>{copy.input}</th><th>{copy.cache}</th><th>{copy.output}</th><th>{copy.totalColumn}</th></tr></thead><tbody>
        {days.map((day) => <tr key={day.date}><th scope="row">{dateLabel(day.date, lang, true)}</th><td>{exact(day.inputTokens, lang)}</td><td>{exact(day.cachedInputTokens, lang)}</td><td>{exact(day.outputTokens, lang)}</td><td>{exact(day.totalTokens, lang)}</td></tr>)}
      </tbody></table></div>
    </details>
  </section>;
}

function ModelDistribution({ models, lang, copy, onModel }: { models: UsageModel[]; lang: Language; copy: UsageCopy; onModel: (model: string) => void }) {
  const rows = [...models].sort((a, b) => b.totalTokens - a.totalTokens);
  const total = rows.reduce((sum, model) => sum + model.totalTokens, 0);
  const circumference = 2 * Math.PI * 58;
  let cumulative = 0;
  return <section className="cx-usage-card cx-usage-models">
    <div className="cx-usage-card-heading"><div><h3>{copy.modelTitle}</h3><p>{copy.modelHint}</p></div></div>
    <div className="cx-usage-donut-wrap"><svg className="cx-usage-donut" viewBox="0 0 160 160" role="img" aria-label={`${copy.modelTitle}: ${rows.map((row) => `${modelLabel(row.model, lang)} ${percentage(ratio(row.totalTokens, total))}`).join(", ")}`}>
      <circle cx="80" cy="80" r="58" fill="none" className="cx-usage-donut-track" strokeWidth="15" />
      {rows.map((row, index) => {
        const fraction = total > 0 ? row.totalTokens / total : 0;
        const offset = cumulative;
        cumulative += fraction;
        return <circle key={row.model} cx="80" cy="80" r="58" fill="none" strokeWidth="15" strokeDasharray={`${Math.max(0, fraction * circumference - (rows.length > 1 ? 3 : 0))} ${circumference}`} strokeDashoffset={-offset * circumference} transform="rotate(-90 80 80)" className={`cx-usage-model-color-${index % 5}`}><title>{modelLabel(row.model, lang)}: {exact(row.totalTokens, lang)} {copy.tokens}</title></circle>;
      })}
    </svg><div className="cx-usage-donut-label"><strong>{rows.length}</strong><span>{copy.modelCount}</span></div></div>
    <div className="cx-usage-model-list">
      {rows.length === 0 && <p className="cx-usage-muted">{copy.noModels}</p>}
      {rows.map((row, index) => <button key={row.model} type="button" className="cx-usage-model-row" onClick={() => onModel(row.model)} title={`${copy.modelFilter}: ${modelLabel(row.model, lang)}`}>
        <span className={`cx-usage-model-dot cx-usage-model-color-${index % 5}`} aria-hidden="true" />
        <span className="cx-usage-model-name">{modelLabel(row.model, lang)}</span>
        <span className="cx-usage-model-amount"><TokenValue value={row.totalTokens} lang={lang} /><small>{percentage(ratio(row.totalTokens, total))}</small></span>
      </button>)}
    </div>
  </section>;
}

export function UsageStatisticsPage({ lang, configDir, active = true }: UsageStatisticsPageProps) {
  const copy = copyFor(lang);
  const [range, setRange] = useState<UsageRange>("7d");
  const [model, setModel] = useState("");
  const [loadState, dispatch] = useReducer(usageLoadReducer, initialUsageLoadState);
  const requestRef = useRef(0);
  const previousDir = useRef(configDir);
  const query = useMemo(() => ({ configDir, range, model }), [configDir, range, model]);
  const record = loadState.record?.query.configDir === configDir ? loadState.record : null;
  const data = record?.data ?? null;
  const currentRequest = sameUsageQuery(loadState.query, query);
  const busy = active && (!currentRequest || loadState.busy);
  const error = currentRequest ? loadState.error : "";
  const displayingPrevious = record !== null && !sameUsageQuery(record.query, query);
  const queryLabel = (value: typeof query) => `${copy.ranges[value.range]} · ${value.model ? modelLabel(value.model, lang) : copy.allModels}`;
  const statusText = displayingPrevious
    ? `${error ? copy.failedResults : copy.loadingResults} ${queryLabel(query)} · ${copy.previousResults} ${queryLabel(record.query)}`
    : busy ? copy.updatingResults : `${copy.showingResults} ${queryLabel(query)}`;
  const load = useCallback(async (forceRefresh = false) => {
    if (!active) return;
    const request = ++requestRef.current;
    dispatch({ type: "start", requestId: request, query });
    try {
      const next = await invoke<UsageStatistics>("get_usage_statistics", { configDir, range, model: model || null, forceRefresh });
      dispatch({ type: "success", requestId: request, data: next });
    } catch (cause) {
      dispatch({ type: "failure", requestId: request, error: cause instanceof Error ? cause.message : String(cause) });
    }
  }, [active, configDir, range, model, query]);
  useEffect(() => {
    if (previousDir.current !== configDir) {
      previousDir.current = configDir;
      if (model) {
        setModel("");
        return;
      }
    }
    void load();
    return () => { dispatch({ type: "cancel", requestId: ++requestRef.current }); };
  }, [configDir, model, load]);
  const modelOptions = useMemo(() => {
    const available = record?.data.availableModels ?? [];
    return Array.from(new Set([...available, ...(model ? [model] : [])])).sort();
  }, [record, configDir, model]);
  const partial = data && (data.coverage.skippedFiles > 0 || data.coverage.truncated || data.coverage.warnings.length > 0);
  const hasUsage = data && data.totals.totalTokens > 0;
  const cacheRate = data ? ratio(data.totals.cachedInputTokens, data.totals.inputTokens) : 0;

  return <div className="cx-usage" aria-busy={busy}>
    <header className="cx-usage-heading"><div><h3>{copy.title}</h3><p>{copy.description}</p></div><span className="cx-usage-local-badge"><Database size={12} aria-hidden="true" />{copy.source}</span></header>
    <div className="cx-usage-toolbar">
      <div className="cx-usage-range" role="group" aria-label={copy.range}>
        {(Object.keys(copy.ranges) as UsageRange[]).map((value) => <button key={value} type="button" aria-pressed={range === value} className={range === value ? "cx-usage-range-active" : ""} onClick={() => setRange(value)}>{copy.ranges[value]}</button>)}
      </div>
      <div className="cx-usage-toolbar-actions"><label className="cx-usage-model-select"><span className="cx-usage-sr-only">{copy.model}</span><select value={model} onChange={(event) => setModel(event.currentTarget.value)} aria-label={copy.model}><option value="">{copy.allModels}</option>{modelOptions.map((value) => <option key={value} value={value}>{modelLabel(value, lang)}</option>)}</select></label>
        <button type="button" className="cx-page-button cx-page-button--secondary cx-usage-refresh" disabled={busy} onClick={() => void load(true)}><RefreshCw size={14} className={busy ? "cx-page-spin" : ""} aria-hidden="true" />{busy ? copy.refreshing : copy.refresh}</button>
      </div>
      <div className="cx-usage-query-status" role="status" aria-live="polite" aria-atomic="true">
        {busy && data && <Loader2 size={12} className="cx-page-spin" aria-hidden="true" />}
        <span>{data ? statusText : "\u00a0"}</span>
      </div>
    </div>
    {error && <div className="cx-usage-notice cx-usage-notice--error" role="alert"><AlertCircle size={18} aria-hidden="true" /><div><strong>{copy.error}</strong>{data && <p>{copy.stale}</p>}<p>{error}</p></div><button type="button" onClick={() => void load(true)} disabled={busy}>{copy.retry}</button></div>}
    {!data && !error && <div className="cx-usage-loading" role="status"><Loader2 size={27} className="cx-page-spin" aria-hidden="true" /><p>{copy.loading}</p><div className="cx-usage-loading-cards" aria-hidden="true"><span /><span /><span /></div></div>}
    {partial && <div className="cx-usage-notice cx-usage-notice--warning" role="status"><AlertCircle size={18} aria-hidden="true" /><div><strong>{copy.partial}</strong><p>{copy.partialHint}</p>{data.coverage.warnings.length > 0 && <details><summary>{lang === "zh" ? "查看详情" : "View details"}</summary><ul>{data.coverage.warnings.map((warning, index) => <li key={`${index}-${warning}`}>{warning}</li>)}</ul></details>}</div></div>}
    {data && !hasUsage && <section className="cx-usage-empty"><div className="cx-usage-empty-visual" aria-hidden="true"><span /><span /><span /><span /><BarChart3 size={23} /></div><h3>{copy.noData}</h3><p>{copy.noDataDescription}</p>{(range !== "all" || model) && <button type="button" className="cx-page-button cx-page-button--secondary" onClick={() => { setRange("all"); setModel(""); }}>{copy.showAll}</button>}</section>}
    {data && hasUsage && <>
      <section className="cx-usage-summary" aria-label={copy.activity}>
        <article className="cx-usage-total">
          <div className="cx-usage-total-heading"><span><Activity size={16} aria-hidden="true" />{copy.total}</span><span className="cx-usage-period-label">{copy.ranges[record?.query.range ?? range]}</span></div>
          <div className="cx-usage-total-value"><TokenValue value={data.totals.totalTokens} lang={lang} /><span>{copy.tokens}</span></div>
          <div className="cx-usage-total-caption"><span>{exact(data.totals.totalTokens, lang)} {copy.tokens}</span><span>{copy.totalHint}</span></div>
          <div className="cx-usage-composition" aria-hidden="true"><span className="cx-usage-swatch--input" style={{ width: `${ratio(data.totals.inputTokens - data.totals.cachedInputTokens, data.totals.totalTokens)}%` }} /><span className="cx-usage-swatch--cache" style={{ width: `${ratio(data.totals.cachedInputTokens, data.totals.totalTokens)}%` }} /><span className="cx-usage-swatch--output" style={{ width: `${ratio(data.totals.outputTokens, data.totals.totalTokens)}%` }} /></div>
          <div className="cx-usage-total-footer"><CalendarDays size={12} aria-hidden="true" /><span>{data.rangeStart ? `${dateLabel(data.rangeStart, lang, true)} – ${dateLabel(data.rangeEnd, lang, true)}` : copy.ranges.all}</span></div>
        </article>
        <div className="cx-usage-metrics">
          <Metric icon={ArrowDownLeft} label={copy.input} hint={copy.inputHint} accent="input"><TokenValue value={data.totals.inputTokens} lang={lang} /></Metric>
          <Metric icon={ArrowUpRight} label={copy.output} hint={<>{copy.reasoning} <TokenValue value={data.totals.reasoningTokens} lang={lang} /></>} accent="output"><TokenValue value={data.totals.outputTokens} lang={lang} /></Metric>
          <Metric icon={Database} label={copy.cacheRate} hint={<><TokenValue value={data.totals.cachedInputTokens} lang={lang} /> {copy.cached}</>} accent="cache"><span className="cx-usage-number">{percentage(cacheRate)}</span></Metric>
          <Metric icon={MessageSquare} label={copy.sessions} hint={copy.sessionsHint} accent="sessions"><TokenValue value={data.totals.sessionCount} lang={lang} /></Metric>
        </div>
      </section>
      <div className="cx-usage-charts"><UsageTrend days={data.days} lang={lang} copy={copy} /><ModelDistribution models={data.models} lang={lang} copy={copy} onModel={setModel} /></div>
      <section className="cx-usage-card cx-usage-sessions"><div className="cx-usage-card-heading"><div><h3>{copy.recent}<span className="cx-usage-count">{data.sessions.length}</span></h3><p>{copy.recentHint}</p></div><MessageSquare size={18} aria-hidden="true" /></div>
        <div className="cx-usage-table-scroll"><table className="cx-usage-table"><caption className="cx-usage-sr-only">{copy.recent}</caption><thead><tr><th>{copy.session}</th><th>{copy.model}</th><th>{copy.input}</th><th>{copy.output}</th><th>{copy.totalColumn}</th><th>{copy.lastActive}</th></tr></thead><tbody>{data.sessions.map((session) => <tr key={`${session.id}-${session.model}`}><th scope="row"><span className="cx-usage-session-name" title={sessionTitleLabel(session.title, lang)}>{sessionTitleLabel(session.title, lang)}</span></th><td><span className="cx-usage-session-model" title={modelLabel(session.model, lang)}>{modelLabel(session.model, lang)}</span></td><td><TokenValue value={session.inputTokens} lang={lang} /></td><td><TokenValue value={session.outputTokens} lang={lang} /></td><td className="cx-usage-cell-total"><TokenValue value={session.totalTokens} lang={lang} /></td><td className="cx-usage-cell-date">{timestampLabel(session.lastActiveAt, lang)}</td></tr>)}</tbody></table></div>
      </section>
    </>}
    {data && <footer className="cx-usage-footer"><div><Check size={12} aria-hidden="true" /><span>{copy.updated} {timestampLabel(data.refreshedAt, lang)} · {data.timezone} · {copy.scanned} {exact(data.coverage.scannedFiles, lang)} {copy.files}</span></div><p>{copy.notes}</p></footer>}
  </div>;
}
