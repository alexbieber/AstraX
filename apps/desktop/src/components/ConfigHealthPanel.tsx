import { useId } from "react";
import { AlertTriangle, CheckCircle2, FileSearch, FolderOpen, Loader2, RefreshCw, Wrench } from "lucide-react";
import type { ConfigHealthReport } from "../configHealthTypes";
import { Button } from "./ui";
import "../styles/config-health.css";

export type ConfigHealthPanelProps = {
  lang: "zh" | "en";
  report: ConfigHealthReport | null;
  checking: boolean;
  repairing: boolean;
  error: string;
  onCheck: () => void;
  onRepair: () => void;
  onOpenConfig?: () => void;
};

export function ConfigHealthStatus({ lang, report, checking, repairing, error }: Pick<ConfigHealthPanelProps, "lang" | "report" | "checking" | "repairing" | "error">) {
  const zh = lang === "zh";
  const busy = checking || repairing;
  const hasIssues = report?.status === "issues";
  const healthy = report?.status === "healthy" && !error;
  const tone = error || hasIssues || report?.status === "unavailable" ? "warning" : healthy ? "success" : "neutral";
  const status = repairing ? (zh ? "正在修复" : "Repairing")
    : checking ? (zh ? "正在检查" : "Checking")
      : error ? (zh ? "操作未完成" : "Action incomplete")
        : healthy ? (zh ? "配置正常" : "Looks good")
          : hasIssues ? (zh ? "发现问题" : "Needs attention")
            : report?.status === "missing" ? (zh ? "尚无配置" : "No configuration")
              : report?.status === "unavailable" ? (zh ? "暂时无法检查" : "Unavailable")
                : (zh ? "未检查" : "Not checked");
  const StatusIcon = busy ? Loader2 : tone === "warning" ? AlertTriangle : healthy ? CheckCircle2 : FileSearch;
  return <span className={`cx-config-health-status cx-config-health-status--${tone}`} role="status" aria-live="polite">
    <StatusIcon size={14} aria-hidden="true" className={busy ? "cx-config-health-spinner" : undefined} />
    {status}
  </span>;
}

export function ConfigHealthPanel({ lang, report, checking, repairing, error, onCheck, onRepair, onOpenConfig }: ConfigHealthPanelProps) {
  const titleId = useId();
  const zh = lang === "zh";
  const busy = checking || repairing;
  const hasIssues = report?.status === "issues";
  const healthy = report?.status === "healthy" && !error;
  const manualIssues = hasIssues && report.issues.some((issue) => !issue.repairable);

  return <section className="cx-config-health" aria-labelledby={titleId}>
    <div className="cx-config-health-heading">
      <span className="cx-config-health-icon" aria-hidden="true"><FileSearch size={19} /></span>
      <div className="cx-config-health-heading-copy">
        <h3 id={titleId}>{zh ? "配置检查与修复" : "Configuration check & repair"}</h3>
        <p>{zh ? "检查影响 Codex 启动和供应商使用的配置问题，由你决定是否修复。" : "Check for configuration issues that affect Codex and your providers. You choose whether to repair them."}</p>
      </div>
      <ConfigHealthStatus lang={lang} report={report} checking={checking} repairing={repairing} error={error} />
    </div>

    {error && <p className="cx-config-health-error" role="alert">{error}</p>}

    {hasIssues && <div className="cx-config-health-details">
      <ul className="cx-config-health-issues">
        {report.issues.map((issue, index) => <li key={`${issue.code}-${index}`}>
          <AlertTriangle size={15} aria-hidden="true" />
          <div><strong>{issue.title}</strong><p>{issue.description}</p></div>
        </li>)}
      </ul>
      {report.canRepair && report.repairSummary.length > 0 && <div className="cx-config-health-plan">
        <strong>{zh ? "点击修复后，将进行以下调整：" : "Repair will make these changes:"}</strong>
        <ul>{report.repairSummary.map((summary, index) => <li key={index}>{summary}</li>)}</ul>
      </div>}
      {manualIssues && <p className="cx-config-health-note">{zh ? "部分问题需要手动检查。为保留你的配置，软件不会猜测或覆盖无法确认的内容。" : "Some issues need a manual check. Settings that cannot be determined safely will be left for you to review."}</p>}
    </div>}

    {report?.status === "missing" && <p className="cx-config-health-note">{zh ? "还没有找到配置文件。完成 Codex 登录或启用供应商后，可以再次检查。" : "No configuration file was found. Check again after signing in to Codex or enabling a provider."}</p>}
    {report?.status === "unavailable" && !error && <p className="cx-config-health-note">{report.issues[0]?.description || (zh ? "暂时无法读取配置，请稍后重试或打开配置查看。" : "The configuration could not be read. Try again later or open it to review.")}</p>}

    <div className="cx-config-health-footer">
      <p className="cx-config-health-note">{healthy
        ? (zh ? "暂未发现需要修复的配置问题。" : "No configuration issues were found.")
        : (zh ? "检查不会修改配置。点击修复后，会先自动备份。" : "Checking does not change your settings. Choosing repair creates a backup first.")}</p>
      <div className="cx-config-health-actions">
        {onOpenConfig && (hasIssues || report?.status === "unavailable") && <Button variant="ghost" size="sm" icon={<FolderOpen size={15} />} disabled={busy} onClick={onOpenConfig}>{zh ? "查看配置" : "View configuration"}</Button>}
        <Button variant="secondary" size="sm" icon={checking ? <Loader2 size={15} className="cx-config-health-spinner" /> : <RefreshCw size={15} />} disabled={busy} onClick={onCheck}>{checking ? (zh ? "检查中" : "Checking") : (zh ? "检查配置" : "Check configuration")}</Button>
        {report?.canRepair && hasIssues && <Button size="sm" icon={repairing ? <Loader2 size={15} className="cx-config-health-spinner" /> : <Wrench size={15} />} disabled={busy} onClick={onRepair}>{repairing ? (zh ? "修复中" : "Repairing") : (zh ? "修复配置" : "Repair configuration")}</Button>}
      </div>
    </div>
  </section>;
}
