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
  const zh = lang === "zh";
  return <aside className="cx-config-health-toast" aria-label={zh ? "配置检查提醒" : "Configuration check notification"}>
    <span className="cx-config-health-toast-icon" aria-hidden="true"><AlertTriangle size={20} /></span>
    <div className="cx-config-health-toast-content">
      <div role="status" aria-live="polite" aria-atomic="true">
        <strong>{repairing ? (zh ? "正在修复 Codex 配置" : "Repairing Codex configuration") : (zh ? "检测到 Codex 配置问题" : "Codex configuration needs attention")}</strong>
        <p>{(report.canRepair ? report.issues[0]?.title : report.issues[0]?.description) || (zh ? "部分配置可能影响 Codex 正常使用。" : "Some settings may prevent Codex from working correctly.")}</p>
        {report.canRepair && report.repairSummary[0] && <p className="cx-config-health-toast-plan">{report.repairSummary[0]}</p>}
      </div>
      <div className="cx-config-health-toast-actions">
        {report.canRepair && <Button size="sm" icon={repairing ? <Loader2 size={15} className="cx-config-health-spinner" /> : <Wrench size={15} />} disabled={repairing} onClick={onRepair}>{repairing ? (zh ? "修复中" : "Repairing") : (zh ? "修复配置" : "Repair configuration")}</Button>}
        <Button variant={report.canRepair ? "ghost" : "primary"} size="sm" icon={<ArrowRight size={15} />} iconPosition="end" disabled={repairing} onClick={onOpenSettings}>{zh ? "查看设置" : "View settings"}</Button>
      </div>
      <p className="cx-config-health-toast-hint">{zh ? "也可稍后前往「设置 → 通用设置 → 环境与配置检查」处理。" : "You can also use Settings → General → Environment & configuration check later."}</p>
    </div>
    <button type="button" className="cx-config-health-toast-close" disabled={repairing} onClick={onDismiss} aria-label={zh ? "关闭配置检查提醒" : "Dismiss configuration notification"} title={zh ? "关闭提醒" : "Dismiss"}><X size={17} aria-hidden="true" /></button>
  </aside>;
}
