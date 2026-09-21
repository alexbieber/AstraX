import type { ReactNode } from "react";
import {
  AlertCircle,
  CheckCircle2,
  Download,
  Loader2,
  RefreshCw,
  RotateCcw,
  Settings,
  Sparkles,
} from "lucide-react";

import { INITIAL_APP_UPDATER_STATE, type AppUpdaterState } from "../appUpdater";
import type { Lang, StartupDiagnostics } from "../types";
import { Button, ModalShell } from "./ui";

export type AppToastProps = {
  lang: Lang;
  message: string;
  error: string;
  loading?: boolean;
  onDismissMessage: () => void;
  onDismissError: () => void;
};

export function AppToast({
  lang,
  message,
  error,
  loading = false,
  onDismissMessage,
  onDismissError,
}: AppToastProps) {
  const activeText = error || message;
  if (!activeText) return null;

  const isError = Boolean(error);
  const status = loading ? "loading" : isError ? "error" : "success";
  const [firstLine, ...remainingLines] = activeText.split("\n");
  const detail = remainingLines.join("\n").trim();
  const dismiss = isError ? onDismissError : onDismissMessage;

  return (
    <div
      key={`${status}:${activeText}`}
      className={`cx-app-toast cx-app-toast--${status}`}
      role={isError ? "alert" : "status"}
      aria-live={isError ? "assertive" : "polite"}
      onAnimationEnd={(event) => {
        if (event.target !== event.currentTarget || event.animationName !== "cx-app-toast-exit") return;
        dismiss();
      }}
    >
      {loading
        ? <Loader2 className="cx-app-toast-loader" size={18} aria-hidden="true" />
        : <span className="cx-app-toast-dot" aria-hidden="true" />}
      <div className="cx-app-toast-copy">
        <strong>{firstLine || (isError ? "Action failed" : "AstraX")}</strong>
        {detail && <span>{detail}</span>}
      </div>
    </div>
  );
}

export type UpdateDialogProps = {
  open: boolean;
  lang: Lang;
  state?: AppUpdaterState;
  currentVersion?: string | null;
  latestVersion?: string | null;
  onClose: () => void;
  onDownload: () => void;
  onUpdate?: () => void | Promise<unknown>;
  onRetry?: () => void | Promise<unknown>;
  onRestart?: () => void | Promise<unknown>;
};

function formatUpdateBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

export function UpdateDialog({
  open,
  lang,
  state,
  currentVersion,
  latestVersion,
  onClose,
  onDownload,
  onUpdate,
  onRetry,
  onRestart,
}: UpdateDialogProps) {
  const updaterState = state ?? {
    ...INITIAL_APP_UPDATER_STATE,
    phase: "available" as const,
    currentVersion: currentVersion ?? null,
    latestVersion: latestVersion ?? null,
  };
  const phase = updaterState.phase;
  const isBusy = phase === "downloading" || phase === "installing";
  const totalBytes = updaterState.totalBytes;
  const hasKnownProgress = totalBytes !== null && totalBytes > 0;
  const progress = totalBytes !== null && totalBytes > 0
    ? Math.min(100, Math.round((updaterState.downloadedBytes / totalBytes) * 100))
    : null;

  const copy = {
        checkingTitle: "Checking for updates",
        checkingDescription: "Checking whether a new version is available.",
        availableTitle: "New version available",
        availableDescription: onUpdate
          ? "Update directly in the app without downloading the installer again."
          : "A new version is available from the download page for your platform.",
        downloadingTitle: "Downloading update",
        downloadingDescription: "Keep AstraX open. Installation starts automatically after download.",
        installingTitle: "Installing update",
        installingDescription: "Almost done. Please keep the app open.",
        readyTitle: "Update is ready",
        readyDescription: "Restart AstraX to use the new version.",
        errorTitle: "Update did not finish",
        errorDescription: updaterState.failure === "restart"
          ? "AstraX could not restart. Please try again."
          : "Try again, or use the download page if the problem continues.",
        idleTitle: "AstraX is up to date",
        idleDescription: "There is no new version available right now.",
        current: "Current",
        latest: "New version",
        later: "Later",
        close: "Close",
        updateNow: "Update now",
        downloading: "Downloading",
        installing: "Installing",
        restart: "Restart",
        retry: "Try again",
        downloadPage: "Open download page",
        releaseNotes: "What's new",
      };

  const title = phase === "checking"
    ? copy.checkingTitle
    : phase === "available"
      ? copy.availableTitle
      : phase === "downloading"
        ? copy.downloadingTitle
        : phase === "installing"
          ? copy.installingTitle
          : phase === "ready"
            ? copy.readyTitle
            : phase === "error"
              ? copy.errorTitle
              : copy.idleTitle;
  const description = phase === "checking"
    ? copy.checkingDescription
    : phase === "available"
      ? copy.availableDescription
      : phase === "downloading"
        ? copy.downloadingDescription
        : phase === "installing"
          ? copy.installingDescription
          : phase === "ready"
            ? copy.readyDescription
            : phase === "error"
              ? copy.errorDescription
              : copy.idleDescription;

  const handleClose = () => {
    if (!isBusy) onClose();
  };

  const footer = phase === "available"
    ? (
        <>
          <Button variant="secondary" onClick={handleClose}>{copy.later}</Button>
          <Button
            icon={<Download size={16} />}
            onClick={() => {
              if (onUpdate) void onUpdate();
              else onDownload();
            }}
          >
            {onUpdate ? copy.updateNow : copy.downloadPage}
          </Button>
        </>
      )
    : phase === "ready"
      ? (
          <Button icon={<RefreshCw size={16} />} onClick={() => void onRestart?.()}>
            {copy.restart}
          </Button>
        )
      : phase === "error"
        ? (
            <>
              <Button variant="secondary" icon={<Download size={16} />} onClick={onDownload}>
                {copy.downloadPage}
              </Button>
              <Button icon={<RotateCcw size={16} />} onClick={() => void onRetry?.()}>
                {copy.retry}
              </Button>
            </>
          )
        : isBusy
          ? (
              <Button disabled icon={<Loader2 className="spin" size={16} />}>
                {phase === "downloading" ? copy.downloading : copy.installing}
              </Button>
            )
          : <Button variant="secondary" onClick={handleClose}>{copy.close}</Button>;

  return (
    <ModalShell
      open={open}
      onClose={handleClose}
      size="sm"
      title={title}
      description={description}
      closeLabel={copy.close}
      closeOnBackdrop={!isBusy}
      closeOnEscape={!isBusy}
      showCloseButton={!isBusy}
      className="cx-update-dialog"
      footer={footer}
    >
      <div className={`cx-update-dialog-icon cx-update-dialog-icon--${phase}`} aria-hidden="true">
        {phase === "checking" || isBusy
          ? <Loader2 className="spin" size={20} />
          : phase === "ready"
            ? <CheckCircle2 size={20} />
            : phase === "error"
              ? <AlertCircle size={20} />
              : <Sparkles size={20} />}
      </div>
      <dl className="cx-update-version-grid">
        <div><dt>{copy.current}</dt><dd>{updaterState.currentVersion || currentVersion || "-"}</dd></div>
        <div><dt>{copy.latest}</dt><dd>{updaterState.latestVersion || latestVersion || "-"}</dd></div>
      </dl>

      {(phase === "downloading" || phase === "installing" || phase === "ready") && (
        <div className="cx-update-progress" aria-live="polite">
          <div className="cx-update-progress-copy">
            <span>{phase === "downloading" ? copy.downloading : phase === "installing" ? copy.installing : copy.restart}</span>
            <strong>
              {phase === "downloading"
                ? hasKnownProgress
                  ? `${progress}% · ${formatUpdateBytes(updaterState.downloadedBytes)} / ${formatUpdateBytes(totalBytes)}`
                  : updaterState.downloadedBytes > 0
                    ? formatUpdateBytes(updaterState.downloadedBytes)
                    : "..."
                : phase === "ready"
                  ? "100%"
                  : "..."}
            </strong>
          </div>
          <div
            className={`cx-update-progress-track${progress === null && phase === "downloading" ? " cx-update-progress-track--indeterminate" : ""}`}
            role="progressbar"
            aria-label={phase === "downloading" ? copy.downloading : copy.installing}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={phase === "ready" ? 100 : progress ?? undefined}
          >
            <span style={{ width: phase === "ready" || phase === "installing" ? "100%" : progress === null ? "38%" : `${progress}%` }} />
          </div>
        </div>
      )}

      {updaterState.notes && phase !== "checking" && (
        <section className="cx-update-notes">
          <strong>{copy.releaseNotes}</strong>
          <p>{updaterState.notes}</p>
        </section>
      )}
    </ModalShell>
  );
}

export type StartupWizardDialogProps = {
  open: boolean;
  closing: boolean;
  mode?: "startup" | "manual";
  lang: Lang;
  diagnostics: StartupDiagnostics | null;
  diagnosticsError?: string;
  configHealthPanel?: ReactNode;
  configDir: string;
  loading: boolean;
  onConfigDirChange: (value: string) => void;
  onRecheck: () => void;
  onSkip: () => void;
  onOpenSettings: () => void;
  onEnter: () => void;
};

export function StartupWizardDialog({
  open,
  closing,
  mode = "startup",
  lang,
  diagnostics,
  diagnosticsError,
  configHealthPanel,
  configDir,
  loading,
  onConfigDirChange,
  onRecheck,
  onSkip,
  onOpenSettings,
  onEnter,
}: StartupWizardDialogProps) {
  const isManual = mode === "manual";
  const recheckButton = (
    <Button
      variant={isManual ? "primary" : "secondary"}
      icon={<RefreshCw size={16} className={loading ? "spin" : undefined} />}
      onClick={onRecheck}
      disabled={loading}
    >
      {loading ? "Checking" : "Recheck"}
    </Button>
  );

  return (
    <ModalShell
      open={open}
      onClose={onSkip}
      size="lg"
      title={"Environment & configuration check"}
      description={isManual
        ? "Review your environment and configuration, and choose whether to repair any issues."
        : "Before you get started, check whether your Codex environment and configuration are ready."}
      showCloseButton={isManual}
      closeLabel={"Close"}
      closeOnBackdrop={isManual}
      closeOnEscape={isManual}
      className={closing ? "cx-startup-dialog cx-startup-dialog--closing" : "cx-startup-dialog"}
      footer={(
        isManual ? (
          <>
            <Button variant="secondary" onClick={onSkip}>{"Close"}</Button>
            {recheckButton}
          </>
        ) : (
          <>
            <Button variant="ghost" onClick={onSkip}>{"Skip"}</Button>
            <Button variant="secondary" icon={<Settings size={16} />} onClick={onOpenSettings}>{"Settings"}</Button>
            <Button icon={<CheckCircle2 size={16} />} onClick={onEnter}>{"Enter AstraX"}</Button>
          </>
        )
      )}
    >
      <div className={`cx-startup-path-control${isManual ? " cx-startup-path-control--manual" : ""}`}>
        <label htmlFor="cx-startup-codex-home">CODEX_HOME</label>
        <input
          id="cx-startup-codex-home"
          value={configDir}
          onChange={(event) => onConfigDirChange(event.target.value)}
          placeholder="~/.codex"
          disabled={loading}
          spellCheck={false}
        />
        {!isManual && recheckButton}
      </div>

      {diagnosticsError && <div className="cx-startup-diagnostics-notice cx-startup-diagnostics-notice--error" role="alert">
        <AlertCircle size={17} aria-hidden="true" />
        <div>
          <strong>{"Environment check did not finish"}</strong>
          <p>{diagnosticsError}</p>
        </div>
      </div>}

      {!diagnostics && !diagnosticsError && <div className="cx-startup-diagnostics-notice" role="status" aria-live="polite">
        {loading ? <Loader2 className="spin" size={17} aria-hidden="true" /> : <RefreshCw size={17} aria-hidden="true" />}
        <p>{loading
          ? "Checking your Codex environment…"
          : "Choose Recheck to see the current environment status."}</p>
      </div>}

      {diagnostics && <div className="cx-startup-checks">
        {diagnostics.items.map((item) => {
          const isOk = item.status === "ok";
          const isManual = item.status === "manual";
          const statusText = isOk
            ? "Detected"
            : isManual
              ? "Manual selection required"
              : "Not found";
          return (
            <article className={`cx-startup-check${isOk ? " cx-startup-check--ok" : isManual ? " cx-startup-check--manual" : ""}`} key={item.key}>
              <div className="cx-startup-check-icon" aria-hidden="true">
                {isOk ? <CheckCircle2 size={17} /> : <AlertCircle size={17} />}
              </div>
              <div>
                <strong>{item.label}</strong>
                <p>{statusText}</p>
                {item.path && <code title={item.path}>{item.path}</code>}
              </div>
            </article>
          );
        })}
      </div>}

      {configHealthPanel && <div className="cx-startup-config-health">{configHealthPanel}</div>}
    </ModalShell>
  );
}
