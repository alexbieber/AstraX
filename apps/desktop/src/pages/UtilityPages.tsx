import { createContext, useContext, useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import {
  CheckCircle2,
  BarChart3,
  Download,
  ExternalLink,
  Globe2,
  Loader2,
  Power,
  RefreshCw,
  Sparkles,
  SlidersHorizontal,
  Shuffle,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { PageTransition } from "../components/PageTransition";
import { Button, ModalShell } from "../components/ui";
import { UsageStatisticsPage } from "./UsageStatisticsPage";
import { ProviderFailoverPage } from "./ProviderFailoverPage";
import "../styles/utility-pages.css";

export type UtilityLanguage = "zh" | "en";
export type UtilityStatusTone = "neutral" | "success" | "warning" | "error";

type PageHeaderProps = {
  eyebrow: ReactNode;
  title: ReactNode;
  description?: ReactNode;
  aside?: ReactNode;
};

function PageHeader({ eyebrow, title, description, aside }: PageHeaderProps) {
  return (
    <header className="cx-page-header">
      <div className="cx-page-header-copy">
        <div className="cx-page-eyebrow">{eyebrow}</div>
        <h2>{title}</h2>
        {description && <p>{description}</p>}
      </div>
      {aside}
    </header>
  );
}

export type TomlConfigPageProps = {
  eyebrow: ReactNode;
  title: ReactNode;
  description: ReactNode;
  loaded: ReactNode;
  isLoaded: boolean;
  preview: ReactNode;
};

export function TomlConfigPage({
  eyebrow,
  title,
  description,
  loaded,
  isLoaded,
  preview,
}: TomlConfigPageProps) {
  return (
    <section className="cx-utility cx-page cx-page--toml">
      <PageHeader
        eyebrow={eyebrow}
        title={title}
        description={description}
        aside={(
          <div className={`cx-page-header-status${isLoaded ? "" : " cx-page-header-status--missing"}`} aria-live="polite">
            <span className="cx-page-status-dot" aria-hidden="true" />
            <span>{loaded}</span>
          </div>
        )}
      />
      <section className="cx-page-panel cx-page-code-panel">
        <div className="cx-page-code-frame">{preview}</div>
      </section>
    </section>
  );
}

export type SettingsCopy = {
  eyebrow: ReactNode;
  title: ReactNode;
  languageTitle: ReactNode;
  languageDescription: ReactNode;
  chineseLabel: ReactNode;
  englishLabel: ReactNode;
  productTitle: ReactNode;
  productDescription: ReactNode;
  productValue: ReactNode;
  recheckTitle: ReactNode;
  recheckDescription: ReactNode;
  recheckLabel: ReactNode;
  restartTitle: ReactNode;
  restartDescription: ReactNode;
  restartLabel: ReactNode;
  restartTargetLabel: ReactNode;
  restartConfirmTitle: ReactNode;
  restartConfirmDescription: ReactNode;
  restartCancelLabel: string;
  restartConfirmLabel: ReactNode;
  restartingLabel: ReactNode;
};

export type SettingsPageProps = {
  lang: UtilityLanguage;
  configDir: string;
  active?: boolean;
  copy: SettingsCopy;
  onLanguageChange: (lang: UtilityLanguage) => void;
  onRecheck: () => void;
  onRestartCodex: () => Promise<boolean>;
  recheckBusy?: boolean;
  restartBusy?: boolean;
  generalRequest?: number;
  configHealthStatus?: ReactNode;
  onChange?: () => void | Promise<void>;
};

// PageTransition keeps the previous content during its exit animation. Context
// still propagates the current activity state so requests stop immediately,
// while the retained usage component keeps its filters between tabs.
const SettingsUsageActiveContext = createContext(false);
const SettingsFailoverActiveContext = createContext(false);

function SettingsUsagePanel({ lang, configDir }: Pick<SettingsPageProps, "lang" | "configDir">) {
  const active = useContext(SettingsUsageActiveContext);
  return <UsageStatisticsPage lang={lang} configDir={configDir} active={active} />;
}

function SettingsFailoverPanel({ lang, configDir, onChange }: Pick<SettingsPageProps, "lang" | "configDir" | "onChange">) {
  const active = useContext(SettingsFailoverActiveContext);
  return <ProviderFailoverPage lang={lang} configDir={configDir} active={active} onChange={onChange} />;
}

type SettingRowProps = {
  icon: LucideIcon;
  title: ReactNode;
  description: ReactNode;
  action: ReactNode;
};

function SettingRow({ icon: Icon, title, description, action }: SettingRowProps) {
  return (
    <div className="cx-page-setting-row">
      <div className="cx-page-setting-icon" aria-hidden="true">
        <Icon size={18} strokeWidth={1.9} />
      </div>
      <div className="cx-page-setting-copy">
        <strong>{title}</strong>
        <p>{description}</p>
      </div>
      <div className="cx-page-setting-action">{action}</div>
    </div>
  );
}

export function SettingsPage({
  lang,
  configDir,
  active = true,
  copy,
  onLanguageChange,
  onRecheck,
  onRestartCodex,
  recheckBusy = false,
  restartBusy = false,
  generalRequest = 0,
  configHealthStatus,
  onChange,
}: SettingsPageProps) {
  const [tab, setTab] = useState<"general" | "usage" | "failover">("general");
  useEffect(() => { setTab("general"); }, [generalRequest]);
  const [usageOpened, setUsageOpened] = useState(false);
  const [failoverOpened, setFailoverOpened] = useState(false);
  const tabId = useId();
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const [restartConfirmOpen, setRestartConfirmOpen] = useState(false);
  const closeRestartConfirm = () => {
    if (!restartBusy) setRestartConfirmOpen(false);
  };
  const confirmRestart = async () => {
    if (restartBusy) return;
    if (await onRestartCodex()) setRestartConfirmOpen(false);
  };

  return (
    <section className="cx-utility cx-page cx-page--settings">
      <PageHeader eyebrow={copy.eyebrow} title={copy.title} />
      <div className="cx-settings-tabs" role="tablist" aria-label={lang === "zh" ? "设置页面" : "Settings pages"}>
        {(["general", "usage", "failover"] as const).map((value, index, tabs) => {
          const Icon = value === "general" ? SlidersHorizontal : value === "usage" ? BarChart3 : Shuffle;
          const select = () => { setTab(value); if (value === "usage") setUsageOpened(true); if (value === "failover") setFailoverOpened(true); };
          return <button key={value} ref={(element) => { tabRefs.current[index] = element; }} type="button" role="tab" id={`${tabId}-${value}-tab`} aria-controls={`${tabId}-${value}-panel`} aria-selected={tab === value} tabIndex={tab === value ? 0 : -1} className="cx-settings-tab" onClick={select} onKeyDown={(event) => {
            if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
            event.preventDefault();
            const next = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
            tabRefs.current[next]?.click();
            tabRefs.current[next]?.focus();
          }}><Icon size={14} aria-hidden="true" />{value === "general" ? (lang === "zh" ? "通用设置" : "General") : value === "usage" ? (lang === "zh" ? "用量统计" : "Usage statistics") : (lang === "zh" ? "路由与故障转移" : "Routing & failover")}</button>;
        })}
      </div>
      <SettingsUsageActiveContext.Provider value={active && tab === "usage"}>
      <SettingsFailoverActiveContext.Provider value={active && tab === "failover"}>
        <PageTransition pageKey={`settings:${tab}`}>
          <div className="cx-settings-panel cx-page-settings-list" role="tabpanel" id={`${tabId}-general-panel`} aria-labelledby={`${tabId}-general-tab`} hidden={tab !== "general"}>
            <SettingRow
              icon={Globe2}
              title={copy.languageTitle}
              description={copy.languageDescription}
              action={(
                <div className="cx-page-segmented" role="group" aria-label={String(copy.languageTitle)}>
                  <button
                    type="button"
                    className={lang === "zh" ? "cx-page-segmented-button cx-page-segmented-button--active" : "cx-page-segmented-button"}
                    onClick={() => onLanguageChange("zh")}
                    aria-pressed={lang === "zh"}
                  >
                    {copy.chineseLabel}
                  </button>
                  <button
                    type="button"
                    className={lang === "en" ? "cx-page-segmented-button cx-page-segmented-button--active" : "cx-page-segmented-button"}
                    onClick={() => onLanguageChange("en")}
                    aria-pressed={lang === "en"}
                  >
                    {copy.englishLabel}
                  </button>
                </div>
              )}
            />

            <SettingRow
              icon={Sparkles}
              title={copy.productTitle}
              description={copy.productDescription}
              action={<span className="cx-page-value-pill">{copy.productValue}</span>}
            />

            <SettingRow
              icon={CheckCircle2}
              title={copy.recheckTitle}
              description={copy.recheckDescription}
              action={(
                <div className="cx-settings-check-actions">
                  {configHealthStatus}
                <button
                  type="button"
                  className="cx-page-button cx-page-button--secondary"
                  onClick={onRecheck}
                  disabled={recheckBusy}
                >
                  {recheckBusy && <Loader2 size={15} className="cx-page-spin" aria-hidden="true" />}
                  {copy.recheckLabel}
                </button>
                </div>
              )}
            />

            <SettingRow
              icon={Power}
              title={copy.restartTitle}
              description={copy.restartDescription}
              action={(
                <button
                  type="button"
                  className="cx-page-button cx-page-button--secondary"
                  onClick={() => setRestartConfirmOpen(true)}
                  disabled={restartBusy}
                >
                  {restartBusy ? <Loader2 size={15} className="cx-page-spin" aria-hidden="true" /> : <Power size={15} aria-hidden="true" />}
                  {restartBusy ? copy.restartingLabel : copy.restartLabel}
                </button>
              )}
            />
          </div>

          <div className="cx-settings-panel" role="tabpanel" id={`${tabId}-usage-panel`} aria-labelledby={`${tabId}-usage-tab`} hidden={tab !== "usage"}>
            {usageOpened && <SettingsUsagePanel lang={lang} configDir={configDir} />}
          </div>
          <div className="cx-settings-panel" role="tabpanel" id={`${tabId}-failover-panel`} aria-labelledby={`${tabId}-failover-tab`} hidden={tab !== "failover"}>
            {failoverOpened && <SettingsFailoverPanel lang={lang} configDir={configDir} onChange={onChange} />}
          </div>
        </PageTransition>
      </SettingsFailoverActiveContext.Provider>
      </SettingsUsageActiveContext.Provider>

      <ModalShell
        open={restartConfirmOpen}
        onClose={closeRestartConfirm}
        title={copy.restartConfirmTitle}
        description={copy.restartConfirmDescription}
        size="sm"
        closeLabel={copy.restartCancelLabel}
        closeOnBackdrop={!restartBusy}
        closeOnEscape={!restartBusy}
        showCloseButton={!restartBusy}
        footer={(
          <>
            <Button variant="secondary" onClick={closeRestartConfirm} disabled={restartBusy} data-initial-focus>
              {copy.restartCancelLabel}
            </Button>
            <Button
              variant="danger"
              icon={restartBusy ? <Loader2 size={16} className="cx-page-spin" /> : <Power size={16} />}
              onClick={() => void confirmRestart()}
              disabled={restartBusy}
            >
              {restartBusy ? copy.restartingLabel : copy.restartConfirmLabel}
            </Button>
          </>
        )}
      >
        <div className="cx-page-restart-target">
          <Power size={18} aria-hidden="true" />
          <strong>{copy.restartTargetLabel}</strong>
        </div>
      </ModalShell>
    </section>
  );
}

export type AboutCopy = {
  eyebrow: ReactNode;
  title: ReactNode;
  appVersionLabel: ReactNode;
  codexVersionLabel: ReactNode;
  codexHomeLabel: ReactNode;
  projectLabel: ReactNode;
  openProjectLabel: ReactNode;
  openIssuesLabel: ReactNode;
  releasesEyebrow: ReactNode;
  releasesTitle: ReactNode;
  releaseStatusLabel: ReactNode;
  latestVersionLabel: ReactNode;
  checkUpdateLabel: ReactNode;
  openReleasesLabel: ReactNode;
};

export type AboutReleaseState = {
  status: ReactNode;
  latestVersion: ReactNode;
  tone?: UtilityStatusTone;
  checking?: boolean;
  canOpenReleases?: boolean;
};

export type AboutPageProps = {
  copy: AboutCopy;
  appVersion: ReactNode;
  codexVersion: ReactNode;
  codexHome: ReactNode;
  projectUrl: ReactNode;
  release: AboutReleaseState;
  onOpenProject: () => void;
  onOpenIssues: () => void;
  onCheckUpdate: () => void;
  onOpenReleases: () => void;
};

type InfoRowProps = {
  label: ReactNode;
  value: ReactNode;
  mono?: boolean;
};

function InfoRow({ label, value, mono = false }: InfoRowProps) {
  return (
    <div className="cx-page-info-row">
      <span>{label}</span>
      <strong className={mono ? "cx-page-info-value cx-page-info-value--mono" : "cx-page-info-value"}>{value}</strong>
    </div>
  );
}

export function AboutPage({
  copy,
  appVersion,
  codexVersion,
  codexHome,
  projectUrl,
  release,
  onOpenProject,
  onOpenIssues,
  onCheckUpdate,
  onOpenReleases,
}: AboutPageProps) {
  const releaseTone = release.tone || "neutral";
  const releaseStatusClass = `cx-page-release-status cx-page-release-status--${releaseTone}`;

  return (
    <section className="cx-utility cx-page cx-page--about">
      <PageHeader eyebrow={copy.eyebrow} title={copy.title} />

      <section className="cx-page-panel cx-page-about-panel">
        <div className="cx-page-info-list">
          <InfoRow label={copy.appVersionLabel} value={appVersion} />
          <InfoRow label={copy.codexVersionLabel} value={codexVersion} />
          <InfoRow label={copy.codexHomeLabel} value={codexHome} mono />
          <InfoRow label={copy.projectLabel} value={projectUrl} mono />
        </div>
        <div className="cx-page-panel-actions">
          <button type="button" className="cx-page-button cx-page-button--secondary" onClick={onOpenProject}>
            <ExternalLink size={15} aria-hidden="true" />
            {copy.openProjectLabel}
          </button>
          <button type="button" className="cx-page-button cx-page-button--secondary" onClick={onOpenIssues}>
            <ExternalLink size={15} aria-hidden="true" />
            {copy.openIssuesLabel}
          </button>
        </div>
      </section>

      <section className="cx-page-panel cx-page-release-panel">
        <div className="cx-page-release-header">
          <div>
            <div className="cx-page-eyebrow cx-page-eyebrow--muted">{copy.releasesEyebrow}</div>
            <h3>{copy.releasesTitle}</h3>
          </div>
          <span className={releaseStatusClass} aria-live="polite">{release.status}</span>
        </div>
        <div className="cx-page-info-list">
          <InfoRow label={copy.releaseStatusLabel} value={release.status} />
          <InfoRow label={copy.latestVersionLabel} value={release.latestVersion} />
        </div>
        <div className="cx-page-panel-actions">
          <button
            type="button"
            className="cx-page-button cx-page-button--primary"
            onClick={onCheckUpdate}
            disabled={release.checking}
          >
            {release.checking ? <Loader2 size={15} className="cx-page-spin" aria-hidden="true" /> : <RefreshCw size={15} aria-hidden="true" />}
            {copy.checkUpdateLabel}
          </button>
          <button
            type="button"
            className="cx-page-button cx-page-button--secondary"
            onClick={onOpenReleases}
            disabled={release.canOpenReleases === false}
          >
            <Download size={15} aria-hidden="true" />
            {copy.openReleasesLabel}
          </button>
        </div>
      </section>
    </section>
  );
}
