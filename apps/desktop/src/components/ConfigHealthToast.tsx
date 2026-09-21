import { AlertTriangle, ArrowRight, Loader2, Wrench, X } from "lucide-react";
import type { ConfigHealthReport } from "../configHealthTypes";
import { Button } from "./ui";
import "../styles/config-health.css";

export type ConfigHealthToastProps = {
  lang: "zh" | "en";
  report: ConfigHealthReport;
  repairing: boolean;
  onRepair: () => void;
  onDismiss: () => void;
  onOpenSettings: () => void;
};

/** The caller owns notification lifetime; CSS never dismisses this toast. */
export function ConfigHealthToast({ lang, report, repairing, onRepair, onDismiss, onOpenSettings }: ConfigHealthToastProps) {
  return <aside className="cx-config-health-toast" aria-label={"Configuration check notification"}>
    <span className="cx-config-health-toast-icon" aria-hidden="true"><AlertTriangle size={20} /></span>
    <div className="cx-config-health-toast-content">
      <div role="status" aria-live="polite" aria-atomic="true">
        <strong>{repairing ? "Repairing Codex configuration" : "Codex configuration needs attention"}</strong>
        <p>{(report.canRepair ? report.issues[0]?.title : report.issues[0]?.description) || "Some settings may prevent Codex from working correctly."}</p>
        {report.canRepair && report.repairSummary[0] && <p className="cx-config-health-toast-plan">{report.repairSummary[0]}</p>}
      </div>
      <div className="cx-config-health-toast-actions">
        {report.canRepair && <Button size="sm" icon={repairing ? <Loader2 size={15} className="cx-config-health-spinner" /> : <Wrench size={15} />} disabled={repairing} onClick={onRepair}>{repairing ? "Repairing" : "Repair configuration"}</Button>}
        <Button variant={report.canRepair ? "ghost" : "primary"} size="sm" icon={<ArrowRight size={15} />} iconPosition="end" disabled={repairing} onClick={onOpenSettings}>{"View settings"}</Button>
      </div>
      <p className="cx-config-health-toast-hint">{"You can also use Settings → General → Environment & configuration check later."}</p>
    </div>
    <button type="button" className="cx-config-health-toast-close" disabled={repairing} onClick={onDismiss} aria-label={"Dismiss configuration notification"} title={"Dismiss"}><X size={17} aria-hidden="true" /></button>
  </aside>;
}
