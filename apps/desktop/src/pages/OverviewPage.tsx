import {
  ArrowUpRight,
  CheckCircle2,
  CircleAlert,
  Code2,
  FileText,
  KeyRound,
  Loader2,
  RefreshCw,
  Sparkles,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import "../styles/overview-page.css";

export type OverviewLanguage = "zh" | "en";

export type OverviewPageProps = {
  lang: OverviewLanguage;
  ready: boolean;
  model?: string | null;
  configDir: string;
  resolvedCodexDir: string;
  configExists: boolean;
  providerLabel?: string | null;
  instructionEnabled: boolean;
  authExists: boolean;
  officialAuthAvailable: boolean;
  configPath?: string | null;
  modelProvider?: string | null;
  instructionPath?: string | null;
  loading: boolean;
  hasUpdate: boolean;
  latestVersion?: string | null;
  onConfigDirChange: (value: string) => void;
  onRefresh: () => void;
  onOpenUpdate: () => void;
};

type StatusCardProps = {
  icon: LucideIcon;
  label: string;
  value: string;
  tone: "success" | "active" | "muted";
};

function StatusCard({ icon: Icon, label, value, tone }: StatusCardProps) {
  return (
    <article className={`cx-overview-status-card cx-overview-status-card--${tone}`}>
      <div className="cx-overview-status-icon" aria-hidden="true">
        <Icon size={19} strokeWidth={1.9} />
      </div>
      <div className="cx-overview-status-copy">
        <span>{label}</span>
        <strong title={value}>{value}</strong>
      </div>
    </article>
  );
}

type ConfigRowProps = {
  label: string;
  value: string;
};

function ConfigRow({ label, value }: ConfigRowProps) {
  return (
    <div className="cx-overview-config-row">
      <span>{label}</span>
      <code title={value}>{value}</code>
    </div>
  );
}

export function OverviewPage({
  lang,
  ready,
  model,
  configDir,
  resolvedCodexDir,
  configExists,
  providerLabel,
  instructionEnabled,
  authExists,
  officialAuthAvailable,
  configPath,
  modelProvider,
  instructionPath,
  loading,
  hasUpdate,
  latestVersion,
  onConfigDirChange,
  onRefresh,
  onOpenUpdate,
}: OverviewPageProps) {
  const text = {
        eyebrow: "CODEX CONFIG MANAGER",
        notConfigured: "Not configured",
        modelMissing: "Model not configured",
        codexHome: "CODEX_HOME",
        directoryPlaceholder: "Leave empty for the default directory",
        load: "Load",
        config: "Config file",
        found: "Found",
        missing: "Not found",
        provider: "Provider",
        official: "Official",
        instruction: "Instructions",
        enabled: "Enabled",
        disabled: "Disabled",
        auth: "Authentication",
        authFile: "auth.json found",
        officialAuth: "Official auth saved",
        noAuth: "Not found",
        updateFound: "New version available",
        updateAvailable: (version: string) => `AstraX ${version} is available`,
        viewUpdate: "View update",
        liveStatus: "LIVE STATUS",
        currentConfig: "Current Codex configuration",
        on: "Instructions enabled",
        off: "Instructions disabled",
        directory: "Directory",
        configPath: "Config",
        model: "Model",
        providerName: "Provider",
        instructionFile: "Instruction file",
        reading: "Loading",
        readFailed: "Load failed",
      };

  const unresolvedStatus = loading ? text.reading : text.readFailed;
  const displayModel = ready ? (model?.trim() || text.modelMissing) : unresolvedStatus;
  const displayProvider = ready
    ? (providerLabel?.trim() || modelProvider?.trim() || text.official)
    : unresolvedStatus;
  const displayModelProvider = ready ? (modelProvider?.trim() || text.notConfigured) : unresolvedStatus;
  const displayDirectory = ready
    ? (resolvedCodexDir.trim() || configDir.trim() || text.notConfigured)
    : (configDir.trim() || unresolvedStatus);
  const displayConfigPath = ready ? (configPath?.trim() || text.notConfigured) : unresolvedStatus;
  const displayInstructionPath = ready ? (instructionPath?.trim() || text.notConfigured) : unresolvedStatus;
  const updateVersion = latestVersion?.trim() || "";
  const homeInputValue = configDir;
  const authAvailable = ready && (authExists || officialAuthAvailable);
  const authStatus = ready
    ? authExists
      ? text.authFile
      : officialAuthAvailable ? text.officialAuth : text.noAuth
    : unresolvedStatus;

  return (
    <section className="cx-overview-page" aria-label={"Overview"}>
      <header className="cx-overview-header">
        <div className="cx-overview-heading">
          <p className="cx-overview-eyebrow">
            <span className="cx-overview-live-dot" aria-hidden="true" />
            {text.eyebrow}
          </p>
          <h2 title={displayModel}>{displayModel}</h2>
        </div>

        <div className="cx-overview-home-control">
          <label htmlFor="cx-overview-codex-home">{text.codexHome}</label>
          <input
            id="cx-overview-codex-home"
            type="text"
            value={homeInputValue}
            onChange={(event) => onConfigDirChange(event.target.value)}
            placeholder={text.directoryPlaceholder}
            disabled={loading}
            spellCheck={false}
            aria-label={text.codexHome}
          />
          <button type="button" onClick={onRefresh} disabled={loading}>
            <RefreshCw size={15} strokeWidth={2} className={loading ? "cx-overview-spin" : undefined} aria-hidden="true" />
            {text.load}
          </button>
        </div>
      </header>

      {hasUpdate && (
        <aside className="cx-overview-update-strip" role="status">
          <div className="cx-overview-update-copy">
            <span className="cx-overview-update-dot" aria-hidden="true" />
            <div>
              <strong>{text.updateFound}</strong>
              {updateVersion && <p>{text.updateAvailable(updateVersion)}</p>}
            </div>
          </div>
          <button type="button" onClick={onOpenUpdate}>
            {text.viewUpdate}
            <ArrowUpRight size={15} strokeWidth={2} aria-hidden="true" />
          </button>
        </aside>
      )}

      <div className="cx-overview-status-grid">
        <StatusCard
          icon={FileText}
          label={text.config}
          value={ready ? (configExists ? text.found : text.missing) : unresolvedStatus}
          tone={ready && configExists ? "success" : "muted"}
        />
        <StatusCard
          icon={Code2}
          label={text.provider}
          value={displayProvider}
          tone={ready && modelProvider ? "active" : "muted"}
        />
        <StatusCard
          icon={Sparkles}
          label={text.instruction}
          value={ready ? (instructionEnabled ? text.enabled : text.disabled) : unresolvedStatus}
          tone={ready && instructionEnabled ? "success" : "muted"}
        />
        <StatusCard
          icon={KeyRound}
          label={text.auth}
          value={authStatus}
          tone={authAvailable ? "success" : "muted"}
        />
      </div>

      <section className="cx-overview-config-panel">
        <div className="cx-overview-panel-heading">
          <div>
            <p className="cx-overview-section-label">{text.liveStatus}</p>
            <h3>{text.currentConfig}</h3>
          </div>
          <span className={`cx-overview-instruction-pill${ready && instructionEnabled ? " cx-overview-instruction-pill--active" : ""}`}>
            {!ready && loading
              ? <Loader2 className="cx-overview-spin" size={14} strokeWidth={2} aria-hidden="true" />
              : !ready
                ? <CircleAlert size={14} strokeWidth={2} aria-hidden="true" />
                : <CheckCircle2 size={14} strokeWidth={2} aria-hidden="true" />}
            {ready ? (instructionEnabled ? text.on : text.off) : unresolvedStatus}
          </span>
        </div>

        <div className="cx-overview-config-list">
          <ConfigRow label={text.directory} value={displayDirectory} />
          <ConfigRow label={text.configPath} value={displayConfigPath} />
          <ConfigRow label={text.model} value={displayModel} />
          <ConfigRow label={text.providerName} value={displayModelProvider} />
          <ConfigRow label={text.instructionFile} value={displayInstructionPath} />
        </div>
      </section>
    </section>
  );
}
