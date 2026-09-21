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
  const busy = checking || repairing;
  const hasIssues = report?.status === "issues";
  const healthy = report?.status === "healthy" && !error;
  const tone = error || hasIssues || report?.status === "unavailable" ? "warning" : healthy ? "success" : "neutral";
  const status = repairing ? "Repairing"
    : checking ? "Checking"
      : error ? "Action incomplete"
        : healthy ? "Looks good"
          : hasIssues ? "Needs attention"
            : report?.status === "missing" ? "No configuration"
              : report?.status === "unavailable" ? "Unavailable"
                : "Not checked";
  const StatusIcon = busy ? Loader2 : tone === "warning" ? AlertTriangle : healthy ? CheckCircle2 : FileSearch;
  return <span className={`cx-config-health-status cx-config-health-status--${tone}`} role="status" aria-live="polite">
    <StatusIcon size={14} aria-hidden="true" className={busy ? "cx-config-health-spinner" : undefined} />
    {status}
  </span>;
}

export function ConfigHealthPanel({ lang, report, checking, repairing, error, onCheck, onRepair, onOpenConfig }: ConfigHealthPanelProps) {
  const titleId = useId();
  const busy = checking || repairing;
  const hasIssues = report?.status === "issues";
  const healthy = report?.status === "healthy" && !error;
  const manualIssues = hasIssues && report.issues.some((issue) => !issue.repairable);

  return <section className="cx-config-health" aria-labelledby={titleId}>
    <div className="cx-config-health-heading">
      <span className="cx-config-health-icon" aria-hidden="true"><FileSearch size={19} /></span>
      <div className="cx-config-health-heading-copy">
        <h3 id={titleId}>{"Configuration check & repair"}</h3>
        <p>{"Check for configuration issues that affect Codex and your providers. You choose whether to repair them."}</p>
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
        <strong>{"Repair will make these changes:"}</strong>
        <ul>{report.repairSummary.map((summary, index) => <li key={index}>{summary}</li>)}</ul>
      </div>}
      {manualIssues && <p className="cx-config-health-note">{"Some issues need a manual check. Settings that cannot be determined safely will be left for you to review."}</p>}
    </div>}

    {report?.status === "missing" && <p className="cx-config-health-note">{"No configuration file was found. Check again after signing in to Codex or enabling a provider."}</p>}
    {report?.status === "unavailable" && !error && <p className="cx-config-health-note">{report.issues[0]?.description || "The configuration could not be read. Try again later or open it to review."}</p>}

    <div className="cx-config-health-footer">
      <p className="cx-config-health-note">{healthy
        ? "No configuration issues were found."
        : "Checking does not change your settings. Choosing repair creates a backup first."}</p>
      <div className="cx-config-health-actions">
        {onOpenConfig && (hasIssues || report?.status === "unavailable") && <Button variant="ghost" size="sm" icon={<FolderOpen size={15} />} disabled={busy} onClick={onOpenConfig}>{"View configuration"}</Button>}
        <Button variant="secondary" size="sm" icon={checking ? <Loader2 size={15} className="cx-config-health-spinner" /> : <RefreshCw size={15} />} disabled={busy} onClick={onCheck}>{checking ? "Checking" : "Check configuration"}</Button>
        {report?.canRepair && hasIssues && <Button size="sm" icon={repairing ? <Loader2 size={15} className="cx-config-health-spinner" /> : <Wrench size={15} />} disabled={busy} onClick={onRepair}>{repairing ? "Repairing" : "Repair configuration"}</Button>}
      </div>
    </div>
  </section>;
}
