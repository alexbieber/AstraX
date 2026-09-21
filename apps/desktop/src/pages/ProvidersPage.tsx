import { useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  Copy,
  Download,
  Eye,
  EyeOff,
  FilePlus2,
  Gauge,
  Loader2,
  PencilLine,
  Plus,
  RefreshCw,
  Trash2,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { Ref } from "react";
import { PageTransition } from "../components/PageTransition";
import { OfficialQuotaDialog } from "../components/OfficialQuotaDialog";
import { OfficialAccountBadge } from "../components/OfficialAccountBadge";
import { ProviderModelMappings, validateProviderModelMappings } from "../components/ProviderModelMappings";
import { ProviderPresetPicker } from "../components/ProviderPresetPicker";
import { PROVIDER_PRESETS, getProviderPreset, getProviderPresetVariant } from "../providerPresets";
import { Button, Checkbox, ModalShell } from "../components/ui";
import type { ProviderMode, ProviderModelMapping } from "../types";
import "../styles/providers-page.css";

export type ProviderRowSource = "official" | "local" | "detected";

export type ProviderRow = {
  id: string;
  source: ProviderRowSource;
  providerName: string;
  baseUrl: string;
  model: string;
  apiKey?: string;
  wireApi: string;
  requiresOpenaiAuth: boolean;
  isCurrent: boolean;
  isDefaultOfficial?: boolean;
  email?: string | null;
  planType?: string | null;
  hasAuth?: boolean;
  canQueryQuota?: boolean;
  sourceLabel?: string;
  editable?: boolean;
  duplicable?: boolean;
  deletable?: boolean;
  testable?: boolean;
  testingKey?: string;
  meta?: ReactNode;
};

export type ProviderFormValue = {
  apiKey: string;
  baseUrl: string;
  providerName: string;
  model: string;
  wireApi: string;
  requiresOpenaiAuth: boolean;
};

export type OfficialFormValue = {
  providerName: string;
  model: string;
  authJson: string;
  configText: string;
};

export type ProviderCopy = {
  eyebrow: string;
  title: string;
  subtitle: string;
  importLabel: string;
  addLabel: string;
  noProviders: string;
  currentLabel: string;
  enableLabel: string;
  testLabel: string;
  editLabel: string;
  duplicateLabel: string;
  removeLabel: string;
  deleteTitle: string;
  deleteDescription: (providerName: string) => string;
  deleteCurrentDescription: (providerName: string) => string;
  deleteCancelLabel: string;
  deleteConfirmLabel: string;
  noBaseUrlLabel: string;
  officialEyebrow: string;
  officialTitle: string;
  officialHint: string;
  officialUrlLabel: string;
  authPathLabel: string;
  officialCurrentLabel: string;
  officialAuthLabel: string;
  officialTomlLabel: string;
  officialSaveLabel: string;
  loadCcSwitchOfficialLabel: string;
  resetOfficialLabel: string;
  resetOfficialTitle: string;
  resetOfficialDescription: string;
  resetOfficialCancelLabel: string;
  resetOfficialConfirmLabel: string;
  cancelLabel: string;
  formEyebrow: string;
  formAddTitle: string;
  formEditTitle: string;
  formHint: string;
  apiConfigTitle: string;
  apiConfigDescription: string;
  apiKeyLabel: string;
  apiKeyPlaceholder: string;
  showApiKeyLabel: string;
  hideApiKeyLabel: string;
  baseUrlLabel: string;
  nameLabel: string;
  modelLabel: string;
  fetchModelsLabel: string;
  fetchingModelsLabel: string;
  chooseModelLabel: (count: number) => string;
  wireApiLabel: string;
  requiresAuthLabel: string;
  authPreviewTitle: string;
  authPreviewDescription: string;
  tomlTitle: string;
  tomlDescription: string;
  resetTomlLabel: string;
  saveLabel: string;
  savingLabel: string;
};

export type ProviderOfficialInfo = {
  officialUrl: ReactNode;
  authPath: ReactNode;
  current: ReactNode;
};

export type ProvidersPageProps = {
  lang: "zh" | "en";
  configDir: string;
  copy: ProviderCopy;
  mode: ProviderMode;
  providerRows: readonly ProviderRow[];
  loading: boolean;
  testingId: string;
  actionBusy?: string;
  editingProviderId: string | null;
  creatingProvider: boolean;
  selectedPresetId: string;
  selectedPresetVariantId: string;
  providerForm: ProviderFormValue;
  providerModelMappings: readonly ProviderModelMapping[];
  onProviderModelMappingsChange: (rows: ProviderModelMapping[]) => void;
  officialForm: OfficialFormValue;
  officialProfileIsDefault: boolean;
  canLoadCurrentOfficial: boolean;
  officialAuthRef?: Ref<HTMLTextAreaElement>;
  officialTomlRef?: Ref<HTMLTextAreaElement>;
  officialInfo: ProviderOfficialInfo;
  providerAuthPreview: ReactNode;
  providerTomlDraft: string;
  providerTomlRef?: Ref<HTMLTextAreaElement>;
  apiKeyVisible: boolean;
  availableModels: readonly string[];
  fetchingModels: boolean;
  onImportCcSwitch: () => void;
  onAddProvider: () => void;
  onPresetSelect: (id: string) => void;
  onPresetVariantSelect: (id: string) => void;
  onLoadCcSwitchOfficial: () => void;
  onLoadCurrentOfficial: () => void;
  onResetOfficial: () => void;
  onEnableProvider: (row: ProviderRow) => void;
  onTestProvider: (row: ProviderRow) => void;
  onEditProvider: (row: ProviderRow) => void;
  onDuplicateProvider: (row: ProviderRow) => void;
  onDeleteProvider: (row: ProviderRow) => Promise<boolean>;
  onCancelMode: () => void;
  onOfficialNameChange: (value: string) => void;
  onOfficialModelChange: (value: string) => void;
  onOfficialAuthChange: (value: string) => void;
  onOfficialConfigChange: (value: string) => void;
  onSaveOfficial: () => void;
  onApiKeyChange: (value: string) => void;
  onBaseUrlChange: (value: string) => void;
  onProviderNameChange: (value: string) => void;
  onProviderModelChange: (value: string) => void;
  onFetchModels: () => void;
  onWireApiChange: (value: string) => void;
  onRequiresAuthChange: (value: boolean) => void;
  onToggleApiKeyVisibility: () => void;
  onProviderTomlDraftChange: (value: string, origin?: "manual" | "context") => void;
  onResetProviderToml: () => void;
  onSaveProvider: () => void;
};

type FieldProps = {
  label: string;
  children: ReactNode;
  className?: string;
};

function Field({ label, children, className }: FieldProps) {
  return (
    <label className={`cx-providers-field${className ? ` ${className}` : ""}`}>
      <span>{label}</span>
      {children}
    </label>
  );
}

type ContextWindowValues = {
  contextWindow: number | null;
  compactTokenLimit: number | null;
};

type ContextWindowConfig = ContextWindowValues & {
  configText: string;
  enabled: boolean;
};

function ContextWindowControl({
  lang,
  configText,
  disabled,
  onConfigChange,
  onBusyChange,
}: {
  lang: "zh" | "en";
  configText: string;
  disabled: boolean;
  onConfigChange: (text: string) => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const [settings, setSettings] = useState<ContextWindowConfig | null>(null);
  const [error, setError] = useState("");
  const [changing, setChanging] = useState(false);
  const currentText = useRef(configText);
  const requestId = useRef(0);
  const previousValues = useRef<ContextWindowValues | null>(null);
  currentText.current = configText;

  useEffect(() => {
    const id = ++requestId.current;
    let mounted = true;
    setError("");
    const timer = setTimeout(() => {
      void invoke<ContextWindowConfig>("update_codex_context_window", {
        configText,
        enabled: null,
        previousValues: null,
      }).then((result) => {
        if (mounted && requestId.current === id && currentText.current === configText) {
          setSettings(result);
        }
      }).catch((cause: unknown) => {
        if (mounted && requestId.current === id && currentText.current === configText) {
          setSettings(null);
          setError(String(cause));
        }
      });
    }, 100);
    return () => {
      mounted = false;
      ++requestId.current;
      clearTimeout(timer);
    };
  }, [configText]);

  const toggle = async (enabled: boolean) => {
    if (disabled || changing || !settings || settings.configText !== configText) return;
    const originalText = configText;
    const id = ++requestId.current;
    setChanging(true);
    onBusyChange(true);
    setError("");
    try {
      const result = await invoke<ContextWindowConfig>("update_codex_context_window", {
        configText: originalText,
        enabled,
        previousValues: enabled ? null : previousValues.current,
      });
      if (requestId.current !== id || currentText.current !== originalText) return;
      previousValues.current = enabled
        ? { contextWindow: settings.contextWindow, compactTokenLimit: settings.compactTokenLimit }
        : null;
      setSettings(result);
      onConfigChange(result.configText);
    } catch (cause: unknown) {
      if (requestId.current === id && currentText.current === originalText) setError(String(cause));
    } finally {
      setChanging(false);
      onBusyChange(false);
    }
  };

  return (
    <div className="cx-providers-context-control">
      <Checkbox
        className="cx-providers-checkbox cx-providers-context-checkbox"
        label={lang === "zh" ? "开启 1M 上下文窗口" : "Enable 1M context window"}
        checked={Boolean(settings?.enabled)}
        onCheckedChange={(enabled) => void toggle(enabled)}
        disabled={disabled || changing || !settings || settings.configText !== configText}
      />
      {error ? (
        <span className="cx-providers-context-error" role="status" title={error}>
          {lang === "zh" ? "请先修正 config.toml 的格式或上下文字段" : "Check the TOML syntax and context settings first"}
        </span>
      ) : settings?.enabled && settings.configText === configText ? (
        <span className="cx-providers-context-hint">
          {settings.compactTokenLimit !== null
            ? lang === "zh"
              ? `自动压缩阈值：${settings.compactTokenLimit.toLocaleString("zh-CN")} tokens`
              : `Auto-compact at ${settings.compactTokenLimit.toLocaleString("en-US")} tokens`
            : lang === "zh" ? "自动压缩使用 Codex 默认值" : "Codex determines the compaction limit"}
        </span>
      ) : (
        <span className="cx-providers-context-hint">
          {lang === "zh" ? "保存后生效，需模型支持" : "Applies on save; requires model support"}
        </span>
      )}
    </div>
  );
}

function ProviderPresetSection({ lang, creatingProvider, selectedPresetId, selectedPresetVariantId, onPresetSelect, onPresetVariantSelect, disabled }: Pick<ProvidersPageProps, "lang" | "creatingProvider" | "selectedPresetId" | "selectedPresetVariantId" | "onPresetSelect" | "onPresetVariantSelect"> & { disabled: boolean }) {
  if (!creatingProvider) return null;
  const preset = getProviderPreset(selectedPresetId);
  const variant = getProviderPresetVariant(selectedPresetId, selectedPresetVariantId);
  return <ProviderPresetPicker lang={lang} presets={PROVIDER_PRESETS} selectedId={selectedPresetId} onSelect={onPresetSelect} disabled={disabled}>
    {variant && <div className="cx-preset-variant-panel">
      {preset && preset.variants.length > 1 && <div className="cx-preset-variants" role="group" aria-label={lang === "zh" ? "接入方式" : "API service"}>
        <span>{lang === "zh" ? "接入方式" : "API service"}</span>
        {preset.variants.map((item) => <button key={item.id} type="button" aria-pressed={item.id === variant.id} disabled={disabled} onClick={() => onPresetVariantSelect(item.id)}>{lang === "zh" ? item.label : item.labelEn}</button>)}
      </div>}
      <p>{lang === "zh" ? variant.note : variant.noteEn}</p>
    </div>}
  </ProviderPresetPicker>;
}

function ProviderAvatar({ row }: { row: ProviderRow }) {
  const initial = row.providerName.trim().slice(0, 1).toUpperCase() || "?";
  const className = [
    "cx-providers-avatar",
    row.source === "official" ? "cx-providers-avatar--official" : "",
    row.isCurrent ? "cx-providers-avatar--current" : "",
  ].filter(Boolean).join(" ");

  return (
    <div className={className} aria-hidden="true">
      {row.source === "official" ? <span className="cx-providers-openai-logo" /> : initial}
    </div>
  );
}

function ActionIconButton({
  icon: Icon,
  label,
  onClick,
  disabled,
  danger = false,
}: {
  icon: LucideIcon;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      className={`cx-providers-icon-button${danger ? " cx-providers-icon-button--danger" : ""}`}
      title={label}
      aria-label={label}
      onClick={onClick}
      disabled={disabled}
    >
      <Icon size={15} strokeWidth={1.9} aria-hidden="true" />
    </button>
  );
}

function ListPage({
  lang,
  configDir,
  copy,
  providerRows,
  loading,
  testingId,
  actionBusy,
  onImportCcSwitch,
  onAddProvider,
  onEnableProvider,
  onTestProvider,
  onEditProvider,
  onDuplicateProvider,
  onDeleteProvider,
}: Pick<ProvidersPageProps, "lang" | "configDir" | "copy" | "providerRows" | "loading" | "testingId" | "actionBusy" | "onImportCcSwitch" | "onAddProvider" | "onEnableProvider" | "onTestProvider" | "onEditProvider" | "onDuplicateProvider" | "onDeleteProvider">) {
  const [providerToDelete, setProviderToDelete] = useState<ProviderRow | null>(null);
  const [quotaSelection, setQuotaSelection] = useState<{ id: string; configDir: string; email: string | null } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const providerActionsBusy = loading || Boolean(actionBusy);
  const quotaProfile = quotaSelection?.configDir === configDir
    ? providerRows.find((row) => row.source === "official" && row.id === quotaSelection.id && row.canQueryQuota && (row.email ?? null) === quotaSelection.email)
    : undefined;

  useEffect(() => {
    if (quotaSelection && !quotaProfile) setQuotaSelection(null);
  }, [quotaSelection, quotaProfile]);

  const closeDeleteDialog = () => {
    if (!deleting) setProviderToDelete(null);
  };

  const confirmDelete = async () => {
    if (!providerToDelete || deleting) return;
    setDeleting(true);
    try {
      if (await onDeleteProvider(providerToDelete)) setProviderToDelete(null);
    } finally {
      setDeleting(false);
    }
  };

  return (
    <>
      <header className="cx-providers-header">
        <div className="cx-providers-header-copy">
          <div className="cx-providers-eyebrow">{copy.eyebrow}</div>
          <h2>{copy.title}</h2>
          <p>{copy.subtitle}</p>
        </div>
        <div className="cx-providers-header-actions">
          <button
            type="button"
            className="cx-providers-button cx-providers-button--secondary"
            onClick={onImportCcSwitch}
            disabled={providerActionsBusy}
          >
            {actionBusy === "importCcSwitch" ? <Loader2 size={15} className="cx-providers-spin" aria-hidden="true" /> : <RefreshCw size={15} aria-hidden="true" />}
            {copy.importLabel}
          </button>
          <button type="button" className="cx-providers-button cx-providers-button--dark" onClick={onAddProvider} disabled={providerActionsBusy}>
            <Plus size={15} aria-hidden="true" />
            {copy.addLabel}
          </button>
        </div>
      </header>

      <div className="cx-providers-list" role="list">
        {providerRows.length === 0 ? (
          <div className="cx-providers-empty" role="status">{copy.noProviders}</div>
        ) : providerRows.map((row) => {
          const testingKey = row.testingKey || `${row.source}-${row.id}`;
          const isTesting = testingId === testingKey;
          const isProtectedOfficial = row.source === "official" && row.isDefaultOfficial === true;
          return (
            <article className={`cx-providers-row${row.isCurrent ? " cx-providers-row--current" : ""}${row.source === "official" ? " cx-providers-row--official" : ""}`} key={`${row.source}-${row.id}-${row.baseUrl}`} role="listitem">
              <ProviderAvatar row={row} />
              <div className="cx-providers-row-content">
                <div className="cx-providers-row-main">
                  <div className="cx-providers-row-title">
                    <strong title={row.providerName}>{row.providerName}</strong>
                    {row.sourceLabel && (
                      <span className={`cx-providers-source-badge${row.source === "official" ? " cx-providers-source-badge--official" : ""}`}>
                        {row.sourceLabel}
                      </span>
                    )}
                  </div>
                  <code title={row.baseUrl || copy.noBaseUrlLabel}>{row.baseUrl || copy.noBaseUrlLabel}</code>
                  {row.source !== "official" && row.meta && <div className="cx-providers-row-meta">{row.meta}</div>}
                </div>
              </div>
              {row.source === "official" && <OfficialAccountBadge lang={lang} email={row.email} planType={row.planType} hasAuth={Boolean(row.hasAuth)} canQueryQuota={Boolean(row.canQueryQuota)} className="cx-providers-login-badge" />}
              <div className="cx-providers-row-actions">
                {row.isCurrent && <span className="cx-providers-current-badge"><span aria-hidden="true" />{copy.currentLabel}</span>}
                <button
                  type="button"
                  className="cx-providers-button cx-providers-button--small cx-providers-button--secondary"
                  onClick={() => onEnableProvider(row)}
                  disabled={providerActionsBusy || row.isCurrent}
                >
                  {copy.enableLabel}
                </button>
                {row.source === "official" && (
                  <ActionIconButton
                    icon={Gauge}
                    label={row.canQueryQuota
                      ? (lang === "zh" ? "查看额度" : "View quota")
                      : row.hasAuth
                        ? (lang === "zh" ? "查看额度：此认证不支持订阅额度查询" : "View quota: these credentials do not support subscription quota queries")
                        : (lang === "zh" ? "查看额度：请先登录官方 Codex" : "View quota: sign in to official Codex first")}
                    onClick={() => setQuotaSelection({ id: row.id, configDir, email: row.email ?? null })}
                    disabled={providerActionsBusy || !row.canQueryQuota}
                  />
                )}
                {row.testable !== false && (
                  <ActionIconButton
                    icon={isTesting ? Loader2 : Activity}
                    label={copy.testLabel}
                    onClick={() => onTestProvider(row)}
                    disabled={providerActionsBusy || isTesting}
                  />
                )}
                {row.editable !== false && (
                  <ActionIconButton icon={PencilLine} label={copy.editLabel} onClick={() => onEditProvider(row)} disabled={providerActionsBusy} />
                )}
                {row.duplicable && (
                  <ActionIconButton icon={Copy} label={copy.duplicateLabel} onClick={() => onDuplicateProvider(row)} disabled={providerActionsBusy} />
                )}
                {(row.deletable || isProtectedOfficial) && (
                  <ActionIconButton
                    icon={Trash2}
                    label={isProtectedOfficial ? (lang === "zh" ? "默认官方配置不可删除" : "The default official profile cannot be deleted") : copy.removeLabel}
                    onClick={() => { if (!isProtectedOfficial) setProviderToDelete(row); }}
                    disabled={providerActionsBusy || isProtectedOfficial}
                    danger={!isProtectedOfficial}
                  />
                )}
              </div>
            </article>
          );
        })}
      </div>

      <OfficialQuotaDialog
        lang={lang}
        configDir={configDir}
        profile={quotaProfile ? { id: quotaProfile.id, name: quotaProfile.providerName, email: quotaProfile.email ?? null, planType: quotaProfile.planType } : null}
        onClose={() => setQuotaSelection(null)}
      />

      <ModalShell
        open={Boolean(providerToDelete)}
        onClose={closeDeleteDialog}
        title={copy.deleteTitle}
        description={providerToDelete
          ? providerToDelete.isCurrent
            ? copy.deleteCurrentDescription(providerToDelete.providerName)
            : copy.deleteDescription(providerToDelete.providerName)
          : undefined}
        size="sm"
        closeLabel={copy.deleteCancelLabel}
        closeOnBackdrop={!deleting}
        closeOnEscape={!deleting}
        showCloseButton={!deleting}
        className="cx-provider-delete-dialog"
        bodyClassName="cx-provider-delete-dialog-body"
        footer={(
          <>
            <Button variant="secondary" onClick={closeDeleteDialog} disabled={deleting} data-initial-focus>
              {copy.deleteCancelLabel}
            </Button>
            <Button variant="danger" icon={deleting ? <Loader2 size={16} className="cx-providers-spin" /> : <Trash2 size={16} />} onClick={() => void confirmDelete()} disabled={deleting}>
              {copy.deleteConfirmLabel}
            </Button>
          </>
        )}
      >
        <div className="cx-provider-delete-warning">
          <span aria-hidden="true"><AlertTriangle size={22} /></span>
          <strong>{providerToDelete?.providerName}</strong>
        </div>
      </ModalShell>
    </>
  );
}

function ModeHeader({ eyebrow, title, description, cancelLabel, onCancel, disabled = false }: { eyebrow: string; title: string; description: string; cancelLabel: string; onCancel: () => void; disabled?: boolean }) {
  return (
    <header className="cx-providers-form-header">
      <div>
        <div className="cx-providers-eyebrow">{eyebrow}</div>
        <h2>{title}</h2>
        <p>{description}</p>
      </div>
      <button type="button" className="cx-providers-button cx-providers-button--secondary" onClick={onCancel} disabled={disabled}>
        <ArrowLeft size={15} aria-hidden="true" />
        {cancelLabel}
      </button>
    </header>
  );
}

function OfficialForm({
  lang,
  copy,
  creatingProvider,
  selectedPresetId,
  selectedPresetVariantId,
  onPresetSelect,
  onPresetVariantSelect,
  officialForm,
  officialProfileIsDefault,
  canLoadCurrentOfficial,
  officialAuthRef,
  officialTomlRef,
  officialInfo,
  loading,
  actionBusy,
  onCancelMode,
  onOfficialNameChange,
  onOfficialModelChange,
  onOfficialAuthChange,
  onOfficialConfigChange,
  onSaveOfficial,
  onLoadCcSwitchOfficial,
  onLoadCurrentOfficial,
  onResetOfficial,
}: Pick<ProvidersPageProps, "lang" | "copy" | "creatingProvider" | "selectedPresetId" | "selectedPresetVariantId" | "onPresetSelect" | "onPresetVariantSelect" | "officialForm" | "officialProfileIsDefault" | "canLoadCurrentOfficial" | "officialAuthRef" | "officialTomlRef" | "officialInfo" | "loading" | "actionBusy" | "onCancelMode" | "onOfficialNameChange" | "onOfficialModelChange" | "onOfficialAuthChange" | "onOfficialConfigChange" | "onSaveOfficial" | "onLoadCcSwitchOfficial" | "onLoadCurrentOfficial" | "onResetOfficial">) {
  const [resetConfirmOpen, setResetConfirmOpen] = useState(false);
  const [contextWindowBusy, setContextWindowBusy] = useState(false);
  const loadingCcSwitch = actionBusy === "loadCcSwitchOfficial";
  const loadingCurrent = actionBusy === "loadCurrentOfficial";
  const formBusy = loading || actionBusy === "loadOfficialDraft" || loadingCcSwitch || loadingCurrent || contextWindowBusy;
  const showDefaultActions = officialProfileIsDefault && !creatingProvider;
  const profileHint = lang === "zh"
    ? "认证内容可留空；保存并启用后，在 Codex 中完成登录。复制官方供应商会保留原认证，并可修改名称。"
    : "Authentication may be left empty. Save and enable this provider, then sign in through Codex. A copied official provider keeps its authentication and can be renamed.";

  const confirmReset = () => {
    if (!showDefaultActions || formBusy) return;
    setResetConfirmOpen(false);
    onResetOfficial();
  };

  return (
    <>
      <ModeHeader
        eyebrow={copy.officialEyebrow}
        title={creatingProvider ? copy.formAddTitle : copy.officialTitle}
        description={showDefaultActions ? `${copy.officialHint} ${profileHint}` : profileHint}
        cancelLabel={copy.cancelLabel}
        onCancel={onCancelMode}
        disabled={formBusy}
      />
      <ProviderPresetSection lang={lang} creatingProvider={creatingProvider} selectedPresetId={selectedPresetId} selectedPresetVariantId={selectedPresetVariantId} onPresetSelect={onPresetSelect} onPresetVariantSelect={onPresetVariantSelect} disabled={formBusy} />
      <div className="cx-providers-info-grid">
        <div><span>{copy.officialUrlLabel}</span><code>{officialInfo.officialUrl}</code></div>
        <div><span>{copy.authPathLabel}</span><code>{officialInfo.authPath}</code></div>
        <div><span>{copy.officialCurrentLabel}</span><code>{officialInfo.current}</code></div>
      </div>
      <div className="cx-providers-form-grid cx-providers-form-grid--single">
        <Field label={copy.nameLabel}><input value={officialForm.providerName} onChange={(event) => onOfficialNameChange(event.target.value)} disabled={formBusy} /></Field>
        <Field label={copy.modelLabel}><input value={officialForm.model} onChange={(event) => onOfficialModelChange(event.target.value)} disabled={formBusy} /></Field>
      </div>
      <div className="cx-providers-editor-field">
        <div className="cx-providers-context-heading">
          <span>{copy.officialTomlLabel}</span>
          <ContextWindowControl lang={lang} configText={officialForm.configText} onConfigChange={onOfficialConfigChange} disabled={formBusy} onBusyChange={setContextWindowBusy} />
        </div>
        <textarea
          ref={officialTomlRef}
          className="cx-providers-code-editor cx-providers-toml-editor"
          value={officialForm.configText}
          aria-label={copy.officialTomlLabel}
          onChange={(event) => onOfficialConfigChange(event.target.value)}
          disabled={formBusy}
          spellCheck={false}
        />
      </div>
      <Field label={copy.officialAuthLabel} className="cx-providers-editor-field">
        <textarea
          ref={officialAuthRef}
          className="cx-providers-code-editor cx-providers-auth-editor"
          value={officialForm.authJson}
          onChange={(event) => onOfficialAuthChange(event.target.value)}
          disabled={formBusy}
          wrap="soft"
          spellCheck={false}
        />
      </Field>
      <div className="cx-providers-form-actions cx-providers-form-actions--save cx-providers-official-actions">
        <button type="button" className="cx-providers-button cx-providers-button--secondary" onClick={onLoadCurrentOfficial} disabled={formBusy || !canLoadCurrentOfficial}>
          {loadingCurrent
            ? <Loader2 size={15} className="cx-providers-spin" aria-hidden="true" />
            : <Download size={15} aria-hidden="true" />}
          {lang === "zh" ? "读取当前官方登录" : "Load current official login"}
        </button>
        <button type="button" className="cx-providers-button cx-providers-button--secondary" onClick={onLoadCcSwitchOfficial} disabled={formBusy}>
          {loadingCcSwitch
            ? <Loader2 size={15} className="cx-providers-spin" aria-hidden="true" />
            : <Download size={15} aria-hidden="true" />}
          {copy.loadCcSwitchOfficialLabel}
        </button>
        {showDefaultActions && (
          <button type="button" className="cx-providers-button cx-providers-button--secondary" onClick={() => setResetConfirmOpen(true)} disabled={formBusy}>
            <FilePlus2 size={15} aria-hidden="true" />{copy.resetOfficialLabel}
          </button>
        )}
        <button type="button" className="cx-providers-button cx-providers-button--primary" onClick={onSaveOfficial} disabled={formBusy}><CheckCircle2 size={15} aria-hidden="true" />{copy.officialSaveLabel}</button>
      </div>
      <ModalShell
        open={resetConfirmOpen && showDefaultActions}
        onClose={() => setResetConfirmOpen(false)}
        title={copy.resetOfficialTitle}
        description={copy.resetOfficialDescription}
        size="sm"
        closeLabel={copy.resetOfficialCancelLabel}
        footer={(
          <>
            <Button variant="secondary" onClick={() => setResetConfirmOpen(false)} data-initial-focus>{copy.resetOfficialCancelLabel}</Button>
            <Button variant="danger" icon={<FilePlus2 size={16} />} onClick={confirmReset}>{copy.resetOfficialConfirmLabel}</Button>
          </>
        )}
      >
        <div className="cx-provider-delete-warning">
          <span aria-hidden="true"><AlertTriangle size={22} /></span>
          <strong>config.toml + auth.json</strong>
        </div>
      </ModalShell>
    </>
  );
}

function ProviderForm({
  lang,
  copy,
  creatingProvider,
  selectedPresetId,
  selectedPresetVariantId,
  onPresetSelect,
  onPresetVariantSelect,
  providerForm,
  providerModelMappings,
  onProviderModelMappingsChange,
  loading,
  editingProviderId,
  providerAuthPreview,
  providerTomlDraft,
  providerTomlRef,
  apiKeyVisible,
  availableModels,
  fetchingModels,
  onCancelMode,
  onApiKeyChange,
  onBaseUrlChange,
  onProviderNameChange,
  onProviderModelChange,
  onFetchModels,
  onWireApiChange,
  onRequiresAuthChange,
  onToggleApiKeyVisibility,
  onProviderTomlDraftChange,
  onResetProviderToml,
  onSaveProvider,
}: Pick<ProvidersPageProps, "lang" | "copy" | "creatingProvider" | "selectedPresetId" | "selectedPresetVariantId" | "onPresetSelect" | "onPresetVariantSelect" | "providerForm" | "providerModelMappings" | "onProviderModelMappingsChange" | "loading" | "editingProviderId" | "providerAuthPreview" | "providerTomlDraft" | "providerTomlRef" | "apiKeyVisible" | "availableModels" | "fetchingModels" | "onCancelMode" | "onApiKeyChange" | "onBaseUrlChange" | "onProviderNameChange" | "onProviderModelChange" | "onFetchModels" | "onWireApiChange" | "onRequiresAuthChange" | "onToggleApiKeyVisibility" | "onProviderTomlDraftChange" | "onResetProviderToml" | "onSaveProvider">) {
  const modelListId = useId();
  const [contextWindowBusy, setContextWindowBusy] = useState(false);
  const canFetchModels = Boolean(providerForm.baseUrl.trim() && providerForm.apiKey.trim());
  const formBusy = loading || fetchingModels || contextWindowBusy;
  const mappingsValid = validateProviderModelMappings(providerModelMappings, providerForm.model, lang).valid;

  return (
    <>
      <ModeHeader
        eyebrow={copy.formEyebrow}
        title={editingProviderId ? copy.formEditTitle : copy.formAddTitle}
        description={copy.formHint}
        cancelLabel={copy.cancelLabel}
        onCancel={onCancelMode}
        disabled={formBusy}
      />
      <ProviderPresetSection lang={lang} creatingProvider={creatingProvider} selectedPresetId={selectedPresetId} selectedPresetVariantId={selectedPresetVariantId} onPresetSelect={onPresetSelect} onPresetVariantSelect={onPresetVariantSelect} disabled={formBusy} />

      <section className="cx-providers-form-section">
        <div className="cx-providers-section-heading">
          <div><h3>{copy.apiConfigTitle}</h3><p>{copy.apiConfigDescription}</p></div>
        </div>
        <div className="cx-providers-form-grid cx-providers-form-grid--provider">
          <Field label={copy.apiKeyLabel} className="cx-providers-field--full">
            <div className="cx-providers-secret-input">
              <input
                type={apiKeyVisible ? "text" : "password"}
                value={providerForm.apiKey}
                onChange={(event) => onApiKeyChange(event.target.value)}
                placeholder={copy.apiKeyPlaceholder}
                disabled={formBusy}
              />
              <button type="button" onClick={onToggleApiKeyVisibility} title={apiKeyVisible ? copy.hideApiKeyLabel : copy.showApiKeyLabel} aria-label={apiKeyVisible ? copy.hideApiKeyLabel : copy.showApiKeyLabel} disabled={formBusy}>
                {apiKeyVisible ? <EyeOff size={15} aria-hidden="true" /> : <Eye size={15} aria-hidden="true" />}
              </button>
            </div>
          </Field>
          <Field label={copy.baseUrlLabel} className="cx-providers-field--full"><input value={providerForm.baseUrl} onChange={(event) => onBaseUrlChange(event.target.value)} disabled={formBusy} /></Field>
          <Field label={copy.nameLabel}><input value={providerForm.providerName} onChange={(event) => onProviderNameChange(event.target.value)} disabled={formBusy} /></Field>
          <Field label={copy.modelLabel}>
            <div className="cx-providers-model-input-row">
              <input
                value={providerForm.model}
                list={availableModels.length ? modelListId : undefined}
                aria-label={copy.modelLabel}
                onChange={(event) => onProviderModelChange(event.target.value)}
                disabled={formBusy}
              />
              <button
                type="button"
                className="cx-providers-button cx-providers-button--secondary cx-providers-button--small cx-providers-fetch-models"
                onClick={onFetchModels}
                disabled={loading || fetchingModels || !canFetchModels}
                title={copy.fetchModelsLabel}
                aria-label={copy.fetchModelsLabel}
              >
                {fetchingModels
                  ? <Loader2 size={14} className="cx-providers-spin" aria-hidden="true" />
                  : <RefreshCw size={14} aria-hidden="true" />}
                {fetchingModels ? copy.fetchingModelsLabel : copy.fetchModelsLabel}
              </button>
            </div>
            {availableModels.length > 0 && (
              <>
                <datalist id={modelListId}>
                  {availableModels.map((model) => <option value={model} key={model} />)}
                </datalist>
                <select
                  className="cx-providers-model-select"
                  value=""
                  aria-label={copy.chooseModelLabel(availableModels.length)}
                  disabled={formBusy}
                  onChange={(event) => {
                    if (event.target.value) onProviderModelChange(event.target.value);
                  }}
                >
                  <option value="">{copy.chooseModelLabel(availableModels.length)}</option>
                  {availableModels.map((model) => <option value={model} key={model}>{model}</option>)}
                </select>
              </>
            )}
          </Field>
          <Field label={copy.wireApiLabel}>
            <select value={providerForm.wireApi} onChange={(event) => onWireApiChange(event.target.value)} disabled={formBusy}>
              <option value="responses">responses</option>
              <option value="chat">chat</option>
            </select>
          </Field>
          <Checkbox
            className="cx-providers-checkbox cx-providers-checkbox--full"
            checked={providerForm.requiresOpenaiAuth}
            onCheckedChange={onRequiresAuthChange}
            label={copy.requiresAuthLabel}
            disabled={formBusy}
          />
        </div>
      </section>

      <ProviderModelMappings
        key={editingProviderId ?? "new-provider"}
        lang={lang}
        rows={providerModelMappings}
        currentModel={providerForm.model}
        availableModels={availableModels}
        disabled={formBusy}
        onChange={onProviderModelMappingsChange}
      />

      <section className="cx-providers-form-section">
        <div className="cx-providers-section-heading"><div><h3>{copy.authPreviewTitle}</h3><p>{copy.authPreviewDescription}</p></div></div>
        <div className="cx-providers-preview">{providerAuthPreview}</div>
      </section>

      <section className="cx-providers-form-section">
        <div className="cx-providers-section-heading cx-providers-section-heading--with-action">
          <div><h3>{copy.tomlTitle}</h3><p>{copy.tomlDescription}</p></div>
          <div className="cx-providers-context-actions">
            <ContextWindowControl lang={lang} configText={providerTomlDraft} onConfigChange={(value) => onProviderTomlDraftChange(value, "context")} disabled={formBusy} onBusyChange={setContextWindowBusy} />
            <button type="button" className="cx-providers-button cx-providers-button--secondary cx-providers-button--small" onClick={onResetProviderToml} disabled={formBusy}><RefreshCw size={14} aria-hidden="true" />{copy.resetTomlLabel}</button>
          </div>
        </div>
        <textarea ref={providerTomlRef} className="cx-providers-code-editor cx-providers-toml-editor" aria-label={copy.tomlTitle} value={providerTomlDraft} onChange={(event) => onProviderTomlDraftChange(event.target.value)} disabled={formBusy} spellCheck={false} />
      </section>

      <div className="cx-providers-form-actions cx-providers-form-actions--save">
        {!mappingsValid && <span className="cx-provider-mappings-save-hint">{lang === "zh" ? "请先修正模型映射中的错误。" : "Correct the model mapping errors before saving."}</span>}
        <button type="button" className="cx-providers-button cx-providers-button--primary" onClick={onSaveProvider} disabled={formBusy || !mappingsValid}>
          {loading ? <Loader2 size={15} className="cx-providers-spin" aria-hidden="true" /> : <CheckCircle2 size={15} aria-hidden="true" />}
          {loading ? copy.savingLabel : copy.saveLabel}
        </button>
      </div>
    </>
  );
}

export function ProvidersPage(props: ProvidersPageProps) {
  const pageKey = props.creatingProvider && props.mode !== "list"
    ? "providers:add"
    : `providers:${props.mode}`;

  return (
    <PageTransition pageKey={pageKey}>
      <section className={`cx-providers cx-page cx-providers--${props.mode}`}>
        {props.mode === "list" && <ListPage {...props} />}
        {props.mode === "official" && <OfficialForm {...props} />}
        {props.mode === "form" && <ProviderForm {...props} />}
      </section>
    </PageTransition>
  );
}
