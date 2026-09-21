import React from "react";
import { flushSync } from "react-dom";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Loader2 } from "lucide-react";
import {
  SessionManagementPage,
  type SessionPreview,
  type SessionSyncStatus,
} from "./pages/SessionManagementPage";
import { OverviewPage } from "./pages/OverviewPage";
import { AboutPage, SettingsPage, TomlConfigPage } from "./pages/UtilityPages";
import { PromptsPage } from "./pages/PromptsPage";
import { SkillsMcpPage, type SkillsMcpNoteKind } from "./pages/SkillsMcpPage";
import { ProvidersPage, type ProviderCopy, type ProviderRow } from "./pages/ProvidersPage";
import { AppShell, type AppTab, type AppTheme } from "./components/AppShell";
import {
  AppToast,
  StartupWizardDialog,
  UpdateDialog,
} from "./components/AppDialogs";
import { PageTransition } from "./components/PageTransition";
import { cx } from "./components/ui";
import { appUpdater, useAppUpdater } from "./appUpdater";
import { providerProfilesMatch, type ProviderProfile } from "./providerProfiles";
import { orderProviderRows } from "./providerRowOrder";
import { createPresetProvider, getProviderPreset, getProviderPresetVariant } from "./providerPresets";
import { validateProviderModelMappings } from "./components/ProviderModelMappings";
import { createOfficialProfileMonitor } from "./officialProfileMonitor";
import { useConfigHealth } from "./useConfigHealth";
import { ConfigHealthPanel, ConfigHealthStatus } from "./components/ConfigHealthPanel";
import { ConfigHealthToast } from "./components/ConfigHealthToast";
import type {
  AboutInfo,
  ActionResult,
  AppUpdateInfo,
  BuiltinPromptDetail,
  BuiltinPromptStatus,
  CodexDesktopRestartResult,
  CodexState,
  ImportResult,
  InstructionMode,
  InstructionTemplate,
  Lang,
  OfficialAuthCandidate,
  OfficialProfileActionResult,
  OfficialProfileDetail,
  OfficialProfileSummary,
  DuplicateProviderResult,
  PromptInjectionMode,
  ProviderConnectionResult,
  ProviderModel,
  ProviderModelsResult,
  ProviderMode,
  ReleaseInfo,
  SavedPrompt,
  SavedProvider,
  SessionDeleteResult,
  SessionSyncResult,
  SkillsMcpActionResult,
  SkillsMcpImportPreview,
  SkillsMcpState,
  StartupDiagnostics,
} from "./types";
import "./styles/base.css";
import "./styles/app-shell.css";
import "./styles/ui-primitives.css";
import "./styles/app-dialogs.css";
import "./styles/dark-theme.css";

type Tab = AppTab;

const THEME_KEY = "codexx.theme";
const STARTUP_WIZARD_SEEN_KEY = "codexx.startupWizardSeen";
const ACTIVE_PROVIDER_KEY = "codexx.activeProviderId";
const PROMPT_INJECTION_MODE_KEY = "codexx.promptInjectionMode";
const FALLBACK_GITHUB_REPO = "alexbieber/AstraX";
const DEFAULT_OFFICIAL_PROFILE_ID = "openai-official";

type ThemeTransitionDocument = Document & {
  startViewTransition?: (update: () => void | Promise<void>) => { finished: Promise<void> };
};

const bundledInstructionTemplates: InstructionTemplate[] = [
  {
    id: "gpt5.5-unrestricted",
    filename: "gpt5.5-unrestricted.md",
    title: "gpt-5.5 unrestricted jailbreak",
    subtitle: "Method: have the AI analyze the project first, then send an indirect reverse-engineering prompt",
    badge: "Recommended",
  },
  {
    id: "gpt5.4-unrestricted",
    filename: "gpt5.4-unrestricted.md",
    title: "gpt-5.4 unrestricted jailbreak",
    subtitle: "Method: have the AI analyze the project first, then send an indirect reverse-engineering prompt",
    badge: "Compatible",
  },
  {
    id: "gpt5.5-jeli",
    filename: "gpt5.5-jeli.md",
    title: "gpt5.5-jeli.md",
    subtitle: "gpt-5.5 plain-language jailbreak (covers ~80% of cases)",
    badge: "General",
  },
  {
    id: "github-gpt-5-6-sol-unrestricted-33b86c71",
    filename: "gpt-5.6-sol-unrestricted.md",
    title: "gpt-5.6-sol-unrestricted.md",
    subtitle: "gpt-5.6-sol jailbreak prompt",
    badge: "Built-in",
  },
  {
    id: "github-3-0-b459e1e8",
    filename: "seagull-3.0.md",
    title: "seagull-3.0.md",
    subtitle: "Test OK: Seagull online — want some fries?",
    badge: "Built-in",
  },
];

const defaultProviderForm: SavedProvider = {
  id: "magicai",
  providerName: "MagicAI",
  baseUrl: "https://sky1818.com",
  model: "gpt-5.5",
  apiKey: "",
  tomlConfig: "",
  wireApi: "responses",
  requiresOpenaiAuth: false,
};

const blankProviderForm: SavedProvider = {
  id: "",
  providerName: "",
  baseUrl: "",
  model: "gpt-5.5",
  apiKey: "",
  tomlConfig: "",
  wireApi: "responses",
  requiresOpenaiAuth: false,
};

const blankPromptForm: SavedPrompt = {
  id: "",
  title: "",
  filename: "",
  content: "",
};

const dict = {
  zh: {
    appSubtitle: "Think · Switch · Build",
    manager: "Codex config manager",
    load: "Load",
    refresh: "Refresh",
    nav: {
      dashboard: "Overview",
      provider: "Provider",
      sessions: "Sessions",
      skillsMcp: "Skills & MCP",
      instruction: "Prompt",
      toml: "TOML",
      settings: "Settings",
      about: "About",
    },
    dashboard: {
      config: "Config",
      found: "Found",
      missing: "Missing",
      provider: "Provider",
      instruction: "Instruction Prompt",
      enabled: "Enabled",
      disabled: "Disabled",
      auth: "Auth",
      currentConfig: "Current Codex config",
      liveStatus: "Live status",
      dir: "Directory",
      configPath: "Config",
      model: "Model",
      providerName: "Provider",
      instructionFile: "Instruction",
      notSet: "Not set",
      officialDefault: "Official / Default",
    },
    provider: {
      title: "Provider list",
      subtitle: "Manage named Codex sign-in profiles and third-party APIs, and switch between them.",
      add: "Add provider",
      importCc: "Import from cc-switch",
      edit: "Edit",
      viewEdit: "Edit",
      remove: "Delete",
      switch: "Switch",
      current: "Current",
      official: "Official",
      noRouting: "No routing",
      authReady: "Auth found",
      authMissing: "Auth missing",
      detected: "Detected from TOML",
      local: "Local",
      noProviders: "No provider yet. Click + to add one.",
      officialEdit: "OpenAI Official settings",
      officialHint: "Official settings are stored separately from the active proxy. Loading a config does not switch providers; switching back uses the saved config first.",
      officialUrl: "Official URL",
      formAdd: "Add provider",
      formEdit: "Edit provider",
      formHint: "Choose a provider type and enter its settings. New profiles are saved to the list; enable one to use it.",
      name: "Provider name",
      baseUrl: "Base URL",
      model: "Model",
      wireApi: "Wire API",
      apiKey: "API Key",
      apiKeyPlaceholder: "Enter API key",
      requiresAuth: "requires_openai_auth",
      save: "Save",
      saveAndSwitch: "Save",
      cancel: "Back",
    },
    instruction: {
      title: "Manage instruction prompt",
      desc: "Enable writes the instruction prompt file and sets model_instructions_file; disable removes AstraX-managed instruction prompt config and deletes the md file. Every write creates a backup first.",
      enabled: "Enabled",
      disabled: "Disabled",
      unset: "model_instructions_file is not set",
      enable: "Enable",
      disable: "Disable / delete",
    },
    toml: {
      title: "Current live TOML config",
      desc: "This is the active ~/.codex/config.toml used by Codex, not a saved provider template. After switching providers, this page shows the newly written live config.",
      loaded: "Loaded",
      missingText: "# config.toml is missing. It will be created after switching or enabling instruction.",
    },
    backups: {
      title: "Backups & restore",
      empty: "No backups yet. A backup will be created before the first write.",
      restore: "Restore",
    },
    settings: {
      title: "Settings",
      language: "Language",
      zh: "Chinese",
      en: "English",
      languageDesc: "The interface is English only.",
      productName: "Product name",
      productDesc: "AstraX is a Codex manager based on Codex-X (MIT), with a Grok-inspired dark UI.",
    },
    loadingConfig: "Reading Codex config...",
    noAuth: "No auth",
    authJson: "auth.json",
  },
  en: {
    appSubtitle: "Think · Switch · Build",
    manager: "Codex config manager",
    load: "Load",
    refresh: "Refresh",
    nav: {
      dashboard: "Overview",
      provider: "Provider",
      sessions: "Sessions",
      skillsMcp: "Skills & MCP",
      instruction: "Prompt",
      toml: "TOML",
      settings: "Settings",
      about: "About",
    },
    dashboard: {
      config: "Config",
      found: "Found",
      missing: "Missing",
      provider: "Provider",
      instruction: "Instruction Prompt",
      enabled: "Enabled",
      disabled: "Disabled",
      auth: "Auth",
      currentConfig: "Current Codex config",
      liveStatus: "Live status",
      dir: "Directory",
      configPath: "Config",
      model: "Model",
      providerName: "Provider",
      instructionFile: "Instruction",
      notSet: "Not set",
      officialDefault: "Official / Default",
    },
    provider: {
      title: "Provider list",
      subtitle: "Manage named Codex sign-in profiles and third-party APIs, and switch between them.",
      add: "Add provider",
      importCc: "Import from cc-switch",
      edit: "Edit",
      viewEdit: "Edit",
      remove: "Delete",
      switch: "Switch",
      current: "Current",
      official: "Official",
      noRouting: "No routing",
      authReady: "Auth found",
      authMissing: "Auth missing",
      detected: "Detected from TOML",
      local: "Local",
      noProviders: "No provider yet. Click + to add one.",
      officialEdit: "OpenAI Official settings",
      officialHint: "Official settings are stored separately from the active proxy. Loading a config does not switch providers; switching back uses the saved config first.",
      officialUrl: "Official URL",
      formAdd: "Add provider",
      formEdit: "Edit provider",
      formHint: "Choose a provider type and enter its settings. New profiles are saved to the list; enable one to use it.",
      name: "Provider name",
      baseUrl: "Base URL",
      model: "Model",
      wireApi: "Wire API",
      apiKey: "API Key",
      apiKeyPlaceholder: "Enter API key",
      requiresAuth: "requires_openai_auth",
      save: "Save",
      saveAndSwitch: "Save",
      cancel: "Back",
    },
    instruction: {
      title: "Manage instruction prompt",
      desc: "Enable writes the instruction prompt file and sets model_instructions_file; disable removes AstraX-managed instruction prompt config and deletes the md file. Every write creates a backup first.",
      enabled: "Enabled",
      disabled: "Disabled",
      unset: "model_instructions_file is not set",
      enable: "Enable",
      disable: "Disable / delete",
    },
    toml: {
      title: "Current live TOML config",
      desc: "This is the active ~/.codex/config.toml used by Codex, not a saved provider template. After switching providers, this page shows the newly written live config.",
      loaded: "Loaded",
      missingText: "# config.toml is missing. It will be created after switching or enabling instruction.",
    },
    backups: {
      title: "Backups & restore",
      empty: "No backups yet. A backup will be created before the first write.",
      restore: "Restore",
    },
    settings: {
      title: "Settings",
      language: "Language",
      zh: "Chinese",
      en: "English",
      languageDesc: "The interface is English only.",
      productName: "Product name",
      productDesc: "AstraX is a Codex manager based on Codex-X (MIT), with a Grok-inspired dark UI.",
    },
    loadingConfig: "Reading Codex config...",
    noAuth: "No auth",
    authJson: "auth.json",
  },
} as const;

function getProviderPageCopy(lang: Lang): ProviderCopy {
  const t = dict[lang];
  return {
    eyebrow: "Provider",
    title: t.provider.title,
    subtitle: t.provider.subtitle,
    importLabel: t.provider.importCc,
    addLabel: t.provider.add,
    noProviders: t.provider.noProviders,
    currentLabel: "Current",
    enableLabel: "Enable",
    testLabel: "Test connection",
    editLabel: t.provider.edit,
    duplicateLabel: "Duplicate provider",
    removeLabel: t.provider.remove,
    deleteTitle: "Delete provider",
    deleteDescription: (providerName) => `“${providerName}” will be removed from the provider list. This cannot be undone.`,
    deleteCurrentDescription: (providerName) => `“${providerName}” is currently active. AstraX will switch to the default official sign-in profile before deleting it. Continue?`,
    deleteCancelLabel: "Cancel",
    deleteConfirmLabel: "Delete",
    noBaseUrlLabel: "no base_url",
    officialEyebrow: "OpenAI Official",
    officialTitle: t.provider.officialEdit,
    officialHint: t.provider.officialHint,
    officialUrlLabel: t.provider.officialUrl,
    authPathLabel: "auth.json",
    officialCurrentLabel: t.provider.current,
    officialAuthLabel: "auth.json (JSON)",
    officialTomlLabel: "config.toml (TOML)",
    officialSaveLabel: "Save official config",
    loadCcSwitchOfficialLabel: "Load from CC Switch",
    resetOfficialLabel: "Reset default official config",
    resetOfficialTitle: "Reset default official config",
    resetOfficialDescription: "This switches to OpenAI Official, removes the live auth.json, and requires a new Codex login. The current files are backed up first.",
    resetOfficialCancelLabel: "Cancel",
    resetOfficialConfirmLabel: "Reset",
    cancelLabel: t.provider.cancel,
    formEyebrow: "Provider",
    formAddTitle: t.provider.formAdd,
    formEditTitle: t.provider.formEdit,
    formHint: t.provider.formHint,
    apiConfigTitle: "Provider API configuration",
    apiConfigDescription: "Manage API, authentication, and config.toml in one place.",
    apiKeyLabel: t.provider.apiKey,
    apiKeyPlaceholder: t.provider.apiKeyPlaceholder,
    showApiKeyLabel: "Show API key",
    hideApiKeyLabel: "Hide API key",
    baseUrlLabel: t.provider.baseUrl,
    nameLabel: t.provider.name,
    modelLabel: t.provider.model,
    fetchModelsLabel: "Fetch models",
    fetchingModelsLabel: "Fetching",
    chooseModelLabel: (count) => `Choose a model (${count})`,
    wireApiLabel: t.provider.wireApi,
    requiresAuthLabel: t.provider.requiresAuth,
    authPreviewTitle: "auth.json (JSON)",
    authPreviewDescription: "Enabling applies the API key using this provider's authentication settings. This previews the contents saved in auth.json.",
    tomlTitle: "config.toml (TOML)",
    tomlDescription: "The standard fields above are authoritative when enabled. Other fields from an existing template are preserved; only Reset replaces it with the standard template.",
    resetTomlLabel: "Reset",
    saveLabel: t.provider.saveAndSwitch,
    savingLabel: "Saving...",
  };
}

function providerId(name: string) {
  const slug = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug || `provider-${Date.now()}`;
}

function isReservedCodexProviderId(id: string) {
  return ["openai", "custom", "amazon-bedrock", "ollama", "lmstudio", "oss"].includes(id.trim().toLowerCase());
}

function customProviderId(name: string) {
  const id = providerId(name);
  return isReservedCodexProviderId(id) ? `${id}-custom` : id;
}

function uniqueId(base: string, existingIds: Iterable<string>) {
  const used = new Set(Array.from(existingIds).map((id) => id.trim().toLowerCase()));
  const clean = providerId(base);
  let candidate = clean;
  let index = 2;
  while (used.has(candidate.toLowerCase())) {
    candidate = `${clean}-${index}`;
    index += 1;
  }
  return candidate;
}

function splitMarkdownFilename(filename: string) {
  const clean = filename.trim().replace(/[\/\\]+/g, "-") || "prompt.md";
  const stem = clean.replace(/\.md$/i, "") || "prompt";
  return { stem, filename: `${stem}.md` };
}

function uniquePromptFilename(filename: string, existingFilenames: Iterable<string>) {
  const used = new Set(Array.from(existingFilenames).map((name) => name.trim().toLowerCase()));
  const { stem } = splitMarkdownFilename(filename);
  let candidate = `${stem}.md`;
  let index = 2;
  while (used.has(candidate.toLowerCase())) {
    candidate = `${stem}-${index}.md`;
    index += 1;
  }
  return candidate;
}

function tomlEscape(value: string) {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function extractOpenAiApiKey(authText?: string) {
  if (!authText?.trim()) return "";
  try {
    const parsed = JSON.parse(authText) as { OPENAI_API_KEY?: unknown };
    return typeof parsed.OPENAI_API_KEY === "string" ? parsed.OPENAI_API_KEY : "";
  } catch {
    return "";
  }
}

function parseTomlStringValue(value: string) {
  const raw = value.trim();
  if (raw.startsWith('"')) {
    try {
      return JSON.parse(raw) as string;
    } catch {
      const end = raw.lastIndexOf('"');
      return end > 0 ? raw.slice(1, end) : raw.slice(1);
    }
  }
  if (raw.startsWith("'")) {
    const end = raw.lastIndexOf("'");
    return end > 0 ? raw.slice(1, end) : raw.slice(1);
  }
  return raw.replace(/\s+#.*$/, "").trim();
}

function extractTomlProviderApiKey(configText: string | undefined, providerId?: string) {
  if (!configText?.trim()) return "";
  const targetSection = providerId ? `model_providers.${providerId}` : "";
  let currentSection = "";
  let topLevelValue = "";
  let firstProviderValue = "";

  for (const line of configText.replace(/\r\n?/g, "\n").split("\n")) {
    const section = line.match(/^\s*\[([^\]]+)]\s*(?:#.*)?$/);
    if (section) {
      currentSection = section[1].trim();
      continue;
    }
    const token = line.match(/^\s*experimental_bearer_token\s*=\s*(.+?)\s*$/);
    if (!token) continue;
    const value = parseTomlStringValue(token[1]).trim();
    if (!value) continue;
    if (!currentSection) topLevelValue = value;
    if (currentSection.startsWith("model_providers.") && !firstProviderValue) firstProviderValue = value;
    if (targetSection && currentSection === targetSection) return value;
  }

  return topLevelValue || (!providerId ? firstProviderValue : "");
}

function extractTomlModelProvider(configText: string | undefined) {
  if (!configText?.trim()) return "";
  for (const line of configText.replace(/\r\n?/g, "\n").split("\n")) {
    if (/^\s*\[/.test(line)) break;
    const entry = line.match(/^\s*model_provider\s*=\s*(.+?)\s*$/);
    if (entry) return parseTomlStringValue(entry[1]).trim();
  }
  return "";
}

function savedProviderApiKey(provider: SavedProvider) {
  const providerId = extractTomlModelProvider(provider.tomlConfig);
  return (provider.apiKey || "").trim()
    || extractTomlProviderApiKey(provider.tomlConfig, providerId || undefined);
}

function savedProviderMatchesProfile(provider: SavedProvider, profile: ProviderProfile) {
  return providerProfilesMatch({
    baseUrl: provider.baseUrl,
    providerName: provider.providerName,
    model: provider.model,
    apiKey: savedProviderApiKey(provider),
  }, profile);
}

function buildProviderTomlPreview(provider: SavedProvider) {
  const model = provider.model.trim();
  const name = provider.providerName.trim();
  const providerKey = "custom";
  const baseUrl = provider.baseUrl.trim().replace(/\/+$/, "");
  if (!model || !name || !baseUrl) return "";
  const wireApi = provider.wireApi || "responses";
  return [
    `model_provider = "${tomlEscape(providerKey)}"`,
    `model = "${tomlEscape(model)}"`,
    "",
    `[model_providers.${providerKey}]`,
    `name = "${tomlEscape(name)}"`,
    `base_url = "${tomlEscape(baseUrl)}"`,
    `wire_api = "${tomlEscape(wireApi)}"`,
    `requires_openai_auth = ${provider.requiresOpenaiAuth ? "true" : "false"}`,
    `supports_websockets = false`,
  ].join("\n");
}

function buildOfficialTomlPreview(model: string) {
  return [
    `model_provider = "custom"`,
    `model = "${tomlEscape(model.trim() || "gpt-5.5")}"`,
    "",
    "[model_providers.custom]",
    `name = "OpenAI"`,
    `wire_api = "responses"`,
    `requires_openai_auth = true`,
    `supports_websockets = true`,
  ].join("\n");
}

function buildProviderAuthPreview(provider: SavedProvider) {
  const key = provider.apiKey?.trim();
  return JSON.stringify(key ? { OPENAI_API_KEY: key } : {}, null, 2);
}

function sessionMismatchCount(status: SessionSyncStatus | null) {
  if (!status?.needsSync) return 0;
  const exactCount = Number.isFinite(status.mismatchedSessions)
    ? status.mismatchedSessions
    : Math.max(status.mismatchedThreads, status.mismatchedRollouts);
  return Math.max(1, exactCount);
}

function instructionIdFromPath(path: string | undefined, templates: InstructionTemplate[]) {
  if (!path) return "";
  const normalized = path.replace(/\\/g, "/");
  const found = templates.find((item) => normalized.toLowerCase().endsWith(item.filename.toLowerCase()));
  return found?.id || "custom";
}

function uniqueBuiltinPromptStatuses(statuses: BuiltinPromptStatus[]) {
  const sourcePriority: Record<string, number> = {
    unavailable: 0,
    bundled: 1,
    cache: 2,
    removed: 2,
    github: 3,
  };
  const seenIds = new Set<string>();
  const seenFilenames = new Set<string>();
  const selected = statuses
    .map((item, index) => ({ item, index }))
    .filter(({ item }) => item.id.trim() && item.filename.trim())
    .sort((a, b) =>
      (sourcePriority[b.item.contentSource] ?? -1) - (sourcePriority[a.item.contentSource] ?? -1)
      || a.index - b.index,
    )
    .filter(({ item }) => {
      const id = item.id.trim().toLowerCase();
      const filename = item.filename.trim().toLowerCase();
      if (seenIds.has(id) || seenFilenames.has(filename)) return false;
      seenIds.add(id);
      seenFilenames.add(filename);
      return true;
    });
  return selected.sort((a, b) => a.index - b.index).map(({ item }) => item);
}

function JsonPreview({ text }: { text: string }) {
  return (
    <pre className="toml-preview json-preview" aria-label="JSON preview">
      {text.split("\n").map((line, index) => (
        <div className="toml-line" key={index}>
          <span className="toml-line-no">{index + 1}</span>
          <code>{line}</code>
        </div>
      ))}
    </pre>
  );
}

function renderTomlValue(value: string, lineKey: string) {
  const parts = value.split(/("(?:\\.|[^"])*")/g);
  return parts.map((part, index) => {
    if (!part) return null;
    const key = `${lineKey}-v-${index}`;
    if (/^"(?:\\.|[^"])*"$/.test(part)) {
      return <span className="toml-string" key={key}>{part}</span>;
    }
    const boolParts = part.split(/\b(true|false)\b/g);
    return boolParts.map((piece, boolIndex) => {
      if (piece === "true" || piece === "false") {
        return <span className="toml-bool" key={`${key}-b-${boolIndex}`}>{piece}</span>;
      }
      return <React.Fragment key={`${key}-t-${boolIndex}`}>{piece}</React.Fragment>;
    });
  });
}

function renderTomlLine(line: string, index: number) {
  const key = `toml-${index}`;
  if (line.trim().startsWith("#")) {
    return <span className="toml-comment">{line}</span>;
  }
  if (/^\s*\[[^\]]+\]\s*$/.test(line)) {
    return <span className="toml-section">{line}</span>;
  }
  const eqIndex = line.indexOf("=");
  if (eqIndex > -1) {
    const left = line.slice(0, eqIndex);
    const right = line.slice(eqIndex + 1);
    return (
      <>
        <span className="toml-key">{left}</span>
        <span className="toml-eq">=</span>
        {renderTomlValue(right, key)}
      </>
    );
  }
  return <>{line}</>;
}

function TomlPreview({ text }: { text: string }) {
  return (
    <pre className="toml-preview" aria-label="TOML preview">
      {text.split("\n").map((line, index) => (
        <div className="toml-line" key={index}>
          <span className="toml-line-no">{index + 1}</span>
          <code>{renderTomlLine(line, index)}</code>
        </div>
      ))}
    </pre>
  );
}

function fitCodeEditorHeight(editor: HTMLTextAreaElement | null, minHeight: number) {
  if (!editor) return;
  editor.style.height = "auto";
  editor.style.height = `${Math.max(minHeight, editor.scrollHeight + 2)}px`;
}

function normalizedConfigDirForComparison(value: string) {
  let normalized = value.trim().replace(/\\/g, "/").replace(/\/+$/, "");
  if (/^(?:[a-z]:\/|\/\/)/i.test(normalized)) normalized = normalized.toLowerCase();
  return normalized;
}

function CodexStateLoading({ lang, loading }: { lang: Lang; loading: boolean }) {
  return (
    <section className="cx-state-loading" role="status" aria-live="polite">
      {loading && <Loader2 className="spin" size={22} aria-hidden="true" />}
      <span>{loading
        ? "Loading Codex configuration..."
        : "Could not load the Codex configuration. Retry from Overview."}</span>
    </section>
  );
}

function App() {
  const [lang] = React.useState<Lang>("en");
  const [theme, setTheme] = React.useState<AppTheme>(() =>
    localStorage.getItem(THEME_KEY) === "light" ? "light" : "dark",
  );
  const t = dict[lang];
  const updater = useAppUpdater();
  const isMacRuntime = navigator.userAgent.toLowerCase().includes("mac");
  const [tab, setTab] = React.useState<Tab>("dashboard");
  const [visitedTabs, setVisitedTabs] = React.useState<Set<Tab>>(() => new Set(["dashboard"]));
  const [providerMode, setProviderMode] = React.useState<ProviderMode>("list");
  const [instructionMode, setInstructionMode] = React.useState<InstructionMode>("list");
  const [promptInjectionMode, setPromptInjectionMode] = React.useState<PromptInjectionMode>(() =>
    localStorage.getItem(PROMPT_INJECTION_MODE_KEY) === "replace" ? "replace" : "append",
  );
  const [skillsMcpTab, setSkillsMcpTab] = React.useState<"mcp" | "skills">("mcp");
  const [editingProviderId, setEditingProviderId] = React.useState<string | null>(null);
  const [editingDetectedProvider, setEditingDetectedProvider] = React.useState(false);
  const [editingPromptId, setEditingPromptId] = React.useState<string | null>(null);
  const [editingBuiltinPrompt, setEditingBuiltinPrompt] = React.useState<BuiltinPromptDetail | null>(null);
  const [savedProviders, setSavedProviders] = React.useState<SavedProvider[]>([]);
  const [officialProfiles, setOfficialProfiles] = React.useState<OfficialProfileSummary[]>([]);
  const [editingOfficialProfileId, setEditingOfficialProfileId] = React.useState<string | null>(DEFAULT_OFFICIAL_PROFILE_ID);
  const [creatingProvider, setCreatingProvider] = React.useState(false);
  const [providerCreationBase, setProviderCreationBase] = React.useState("");
  const [selectedPresetId, setSelectedPresetId] = React.useState("custom");
  const [selectedPresetVariantId, setSelectedPresetVariantId] = React.useState("");
  const [officialAuthDirty, setOfficialAuthDirty] = React.useState(false);
  const [activeProviderId, setActiveProviderId] = React.useState(() => localStorage.getItem(ACTIVE_PROVIDER_KEY) || "");
  const [savedPrompts, setSavedPrompts] = React.useState<SavedPrompt[]>([]);
  const [builtinPromptStatus, setBuiltinPromptStatus] = React.useState<BuiltinPromptStatus[]>([]);
  const [aboutInfo, setAboutInfo] = React.useState<AboutInfo | null>(null);
  const [aboutLoading, setAboutLoading] = React.useState(false);
  const [releaseInfo, setReleaseInfo] = React.useState<ReleaseInfo>({ status: "idle" });
  const [updatePromptOpen, setUpdatePromptOpen] = React.useState(false);
  const [sessionStatus, setSessionStatus] = React.useState<SessionSyncStatus | null>(null);
  const [skillsMcpState, setSkillsMcpState] = React.useState<SkillsMcpState | null>(null);
  const [skillsMcpImportPreview, setSkillsMcpImportPreview] = React.useState<SkillsMcpImportPreview | null>(null);
  const [skillsMcpImportOpen, setSkillsMcpImportOpen] = React.useState(false);
  const [startupDiagnostics, setStartupDiagnostics] = React.useState<StartupDiagnostics | null>(null);
  const [startupDiagnosticsError, setStartupDiagnosticsError] = React.useState("");
  const [startupDiagnosticsLoading, setStartupDiagnosticsLoading] = React.useState(false);
  const [startupCheckMode, setStartupCheckMode] = React.useState<"startup" | "manual">("startup");
  const [startupWizardOpen, setStartupWizardOpen] = React.useState(() => localStorage.getItem(STARTUP_WIZARD_SEEN_KEY) !== "1");
  const [startupClosing, setStartupClosing] = React.useState(false);
  const [sessionQuery, setSessionQuery] = React.useState("");
  const deferredSessionQuery = React.useDeferredValue(sessionQuery);
  const [sessionGroupByCwd, setSessionGroupByCwd] = React.useState(false);
  const [showInternalSessions, setShowInternalSessions] = React.useState(false);
  const [selectedSessionIds, setSelectedSessionIds] = React.useState<string[]>([]);
  const [sessionDeleteConfirmOpen, setSessionDeleteConfirmOpen] = React.useState(false);
  const [sessionDeleteBusy, setSessionDeleteBusy] = React.useState(false);
  const [sessionDeleteSafetyConfirmed, setSessionDeleteSafetyConfirmed] = React.useState(false);
  const [state, setState] = React.useState<CodexState | null>(null);
  const [configDir, setConfigDir] = React.useState("");
  const [configDirDraft, setConfigDirDraft] = React.useState("");
  const [healthConfigDir, setHealthConfigDir] = React.useState("");
  const [settingsGeneralRequest, setSettingsGeneralRequest] = React.useState(0);
  const [loading, setLoading] = React.useState(false);
  const [refreshing, setRefreshing] = React.useState(true);
  const [toast, setToast] = React.useState<string>("");
  const [error, setError] = React.useState<string>("");
  const [providerForm, setProviderForm] = React.useState<SavedProvider>(defaultProviderForm);
  const [providerTomlDraft, setProviderTomlDraft] = React.useState("");
  const [providerTomlDirty, setProviderTomlDirty] = React.useState(false);
  const [providerCommonConfigDirty, setProviderCommonConfigDirty] = React.useState(false);
  const [providerDraftRefreshToken, setProviderDraftRefreshToken] = React.useState(0);
  const [providerApiKeyVisible, setProviderApiKeyVisible] = React.useState(false);
  const [providerTestingId, setProviderTestingId] = React.useState("");
  const [availableProviderModels, setAvailableProviderModels] = React.useState<ProviderModel[]>([]);
  const [providerModelsLoading, setProviderModelsLoading] = React.useState(false);
  const [actionBusy, setActionBusy] = React.useState<string>("");
  const [skillsMcpNoteBusy, setSkillsMcpNoteBusy] = React.useState("");
  const [restartCodexBusy, setRestartCodexBusy] = React.useState(false);
  const [promptSyncing, setPromptSyncing] = React.useState(false);
  const [promptDetailLoading, setPromptDetailLoading] = React.useState(false);
  const [promptCatalogReady, setPromptCatalogReady] = React.useState(false);
  const [promptForm, setPromptForm] = React.useState<SavedPrompt>(blankPromptForm);
  const [officialForm, setOfficialForm] = React.useState({
    providerName: "OpenAI Official",
    model: "gpt-5.5",
    authJson: "",
    configText: buildOfficialTomlPreview("gpt-5.5"),
  });
  const [promptModeHelpOpen, setPromptModeHelpOpen] = React.useState(false);
  const autoUpdateCheckedRef = React.useRef(false);
  const promptImportRef = React.useRef<HTMLInputElement | null>(null);
  const nativeTransferBusyRef = React.useRef(false);
  const [sessionExportBusy, setSessionExportBusy] = React.useState(false);
  const officialAuthEditorRef = React.useRef<HTMLTextAreaElement | null>(null);
  const officialTomlEditorRef = React.useRef<HTMLTextAreaElement | null>(null);
  const providerTomlEditorRef = React.useRef<HTMLTextAreaElement | null>(null);
  const providerModelsRequestRef = React.useRef(0);
  const providerDraftRequestRef = React.useRef(0);
  const providerCreationRequestRef = React.useRef(0);
  const officialDraftRequestRef = React.useRef(0);
  const officialProfilesRequestRef = React.useRef(0);
  const officialProfilesLiveRef = React.useRef(officialProfiles);
  officialProfilesLiveRef.current = officialProfiles;
  const officialMonitorReadyRef = React.useRef(false);
  officialMonitorReadyRef.current = Boolean(state) && !refreshing;
  const savedProvidersRequestRef = React.useRef(0);
  const loadingGenerationRef = React.useRef(0);
  const loadingTokensRef = React.useRef(new Set<number>());
  const actionBusyGenerationRef = React.useRef(0);
  const actionBusyTokensRef = React.useRef(new Map<number, string>());
  const skillsMcpNoteBusyRef = React.useRef("");
  const restartCodexBusyRef = React.useRef(false);
  const promptModeHelpRef = React.useRef<HTMLDivElement | null>(null);
  const promptRefreshRequestRef = React.useRef(0);
  const promptDetailRequestRef = React.useRef(0);
  const refreshRequestRef = React.useRef(0);
  const aboutLoadKeyRef = React.useRef("");
  const sessionAutoLoadKeyRef = React.useRef("");
  const sessionLoadRequestRef = React.useRef(0);
  const promptRefreshInFlightRef = React.useRef<Promise<BuiltinPromptStatus[]> | null>(null);
  const promptAutoRefreshAttemptedRef = React.useRef(false);
  const promptCatalogReadyRef = React.useRef(false);
  const promptModeSyncedRef = React.useRef("");
  const skillsMcpLoadedRef = React.useRef("");
  const skillsMcpAutoLoadAttemptedRef = React.useRef("");
  const skillsMcpRequestRef = React.useRef(0);
  const activeConfigDirKeyRef = React.useRef("");
  const routingWakeRef = React.useRef<() => void>(() => {});
  const routingRequestRefreshRef = React.useRef<() => void>(() => {});
  const routingUiRef = React.useRef({ ready: false, editing: false });
  routingUiRef.current = { ready: Boolean(state) && !refreshing, editing: providerMode !== "list" };

  const themeTransitionTimerRef = React.useRef<number | null>(null);
  const providerTomlPreview = React.useMemo(() => buildProviderTomlPreview(providerForm), [providerForm]);
  const providerAuthPreview = React.useMemo(() => buildProviderAuthPreview(providerForm), [providerForm]);
  const activeBuiltinTemplateId = state?.instructionTemplateKey?.startsWith("builtin:")
    ? state.instructionTemplateKey.slice("builtin:".length)
    : "";
  const instructionTemplates = React.useMemo<InstructionTemplate[]>(() => {
    if (!builtinPromptStatus.length) return bundledInstructionTemplates;
    return builtinPromptStatus
      .filter((item) => item.contentSource !== "removed" || item.id === activeBuiltinTemplateId)
      .map(({ id, filename, title, subtitle, badge }) => ({ id, filename, title, subtitle, badge }));
  }, [activeBuiltinTemplateId, builtinPromptStatus]);
  const missingActiveBuiltinTemplateId = activeBuiltinTemplateId
    && !instructionTemplates.some((item) => item.id === activeBuiltinTemplateId)
    ? activeBuiltinTemplateId
    : "";

  const syncActionBusy = React.useCallback(() => {
    let latestToken = -1;
    let latestAction = "";
    for (const [token, action] of actionBusyTokensRef.current) {
      if (token > latestToken) {
        latestToken = token;
        latestAction = action;
      }
    }
    setActionBusy(latestAction);
  }, []);

  const beginLoading = React.useCallback(() => {
    const token = ++loadingGenerationRef.current;
    loadingTokensRef.current.add(token);
    setLoading(true);
    return token;
  }, []);

  const endLoading = React.useCallback((token: number) => {
    if (!loadingTokensRef.current.delete(token)) return;
    setLoading(loadingTokensRef.current.size > 0);
  }, []);

  const beginActionBusy = React.useCallback((action: string) => {
    const token = ++actionBusyGenerationRef.current;
    actionBusyTokensRef.current.set(token, action);
    setActionBusy(action);
    return token;
  }, []);

  const endActionBusy = React.useCallback((token: number) => {
    if (!actionBusyTokensRef.current.delete(token)) return;
    syncActionBusy();
  }, [syncActionBusy]);

  const clearActionBusy = React.useCallback((action: string) => {
    let changed = false;
    for (const [token, activeAction] of actionBusyTokensRef.current) {
      if (activeAction !== action) continue;
      actionBusyTokensRef.current.delete(token);
      changed = true;
    }
    if (changed) syncActionBusy();
  }, [syncActionBusy]);

  const invalidatePromptDetail = React.useCallback(() => {
    promptDetailRequestRef.current += 1;
    setPromptDetailLoading(false);
  }, []);
  const commitSavedProviders = React.useCallback((providers: SavedProvider[]) => {
    savedProvidersRequestRef.current += 1;
    setSavedProviders(providers);
  }, []);
  const currentInstructionId = instructionIdFromPath(state?.instructionFile, instructionTemplates);
  const releaseStatusLabel = React.useMemo(() => {
    if (updater.state.phase === "downloading") return "Downloading";
    if (updater.state.phase === "installing") return "Installing";
    if (updater.state.phase === "ready") return "Restart required";
    if (releaseInfo.status === "checking") return "Checking";
    if (releaseInfo.status === "error") return "Failed";
    if (releaseInfo.hasUpdate) return "Update found";
    if (releaseInfo.status === "ok") return "Up to date";
    "Idle";
  }, [lang, releaseInfo.hasUpdate, releaseInfo.status, updater.state.phase]);


  React.useLayoutEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem(THEME_KEY, theme);
  }, [theme]);

  React.useEffect(() => () => {
    if (themeTransitionTimerRef.current !== null) {
      window.clearTimeout(themeTransitionTimerRef.current);
    }
    document.documentElement.classList.remove("cx-theme-view-transition", "cx-theme-fallback-transition");
  }, []);

  const toggleTheme = React.useCallback(() => {
    const nextTheme: AppTheme = theme === "dark" ? "light" : "dark";
    const root = document.documentElement;
    const commitTheme = () => {
      flushSync(() => setTheme(nextTheme));
    };

    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      commitTheme();
      return;
    }

    const transitionDocument = document as ThemeTransitionDocument;
    if (typeof transitionDocument.startViewTransition === "function") {
      root.classList.remove("cx-theme-fallback-transition");
      root.classList.add("cx-theme-view-transition");
      try {
        const transition = transitionDocument.startViewTransition(commitTheme);
        const clearTransitionClass = () => root.classList.remove("cx-theme-view-transition");
        void transition.finished.then(clearTransitionClass, clearTransitionClass);
        return;
      } catch {
        root.classList.remove("cx-theme-view-transition");
      }
    }

    root.classList.add("cx-theme-fallback-transition");
    commitTheme();
    if (themeTransitionTimerRef.current !== null) {
      window.clearTimeout(themeTransitionTimerRef.current);
    }
    themeTransitionTimerRef.current = window.setTimeout(() => {
      root.classList.remove("cx-theme-fallback-transition");
      themeTransitionTimerRef.current = null;
    }, 260);
  }, [theme]);

  React.useEffect(() => {
    localStorage.setItem(PROMPT_INJECTION_MODE_KEY, promptInjectionMode);
  }, [promptInjectionMode]);

  React.useEffect(() => {
    activeConfigDirKeyRef.current = normalizedConfigDirForComparison(configDir);
  }, [configDir]);

  React.useEffect(() => {
    if (error) setToast("");
  }, [error]);

  React.useEffect(() => {
    if (!promptModeHelpOpen) return undefined;
    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && promptModeHelpRef.current?.contains(target)) return;
      setPromptModeHelpOpen(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setPromptModeHelpOpen(false);
    };
    document.addEventListener("pointerdown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [promptModeHelpOpen]);

  React.useLayoutEffect(() => {
    if (providerMode !== "form") return;
    fitCodeEditorHeight(providerTomlEditorRef.current, 560);
  }, [providerMode, providerTomlDraft]);

  React.useEffect(() => {
    if (providerMode !== "form") {
      providerDraftRequestRef.current += 1;
      return;
    }
    const requestId = ++providerDraftRequestRef.current;
    if (providerTomlDirty) return;

    const fallback = providerForm.tomlConfig?.trim()
      || state?.configText?.trim()
      || providerTomlPreview;
    if (!providerForm.providerName.trim() || !providerForm.baseUrl.trim() || !providerForm.model.trim()) {
      setProviderTomlDraft(fallback);
      return;
    }

    void invoke<string>("build_provider_toml_draft", {
      provider: {
        ...providerForm,
        id: providerForm.id || customProviderId(providerForm.providerName || providerForm.baseUrl),
      },
      newProvider: creatingProvider,
      configDir: configDir || null,
    })
      .then((draft) => {
        if (requestId === providerDraftRequestRef.current) setProviderTomlDraft(draft);
      })
      .catch(() => {
        if (requestId === providerDraftRequestRef.current) setProviderTomlDraft(fallback);
      });
  }, [configDir, creatingProvider, providerDraftRefreshToken, providerForm, providerMode, providerTomlDirty, providerTomlPreview, state?.configText]);

  React.useEffect(() => {
    if (tab === "provider" && providerMode === "form") return;
    providerModelsRequestRef.current += 1;
    setAvailableProviderModels([]);
    setProviderModelsLoading(false);
    if (providerModelsLoading) setToast("");
  }, [providerMode, providerModelsLoading, tab]);

  React.useEffect(() => {
    if (tab === "provider" && providerMode === "official") return;
    officialDraftRequestRef.current += 1;
    clearActionBusy("loadOfficialDraft");
  }, [clearActionBusy, providerMode, tab]);

  React.useEffect(() => {
    if (providerMode !== "official") return;
    const fit = () => {
      fitCodeEditorHeight(officialTomlEditorRef.current, 360);
      fitCodeEditorHeight(officialAuthEditorRef.current, 420);
    };
    const frame = window.requestAnimationFrame(fit);
    window.addEventListener("resize", fit);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("resize", fit);
    };
  }, [officialForm.authJson, officialForm.configText, providerMode]);

  React.useEffect(() => {
    if (!state || promptModeSyncedRef.current === state.codexDir) return;
    promptModeSyncedRef.current = state.codexDir;
    if (state.instructionInjectionMode) {
      setPromptInjectionMode(state.instructionInjectionMode);
    }
  }, [state]);

  React.useEffect(() => {
    setVisitedTabs((tabs) => {
      if (tabs.has(tab)) return tabs;
      const next = new Set(tabs);
      next.add(tab);
      return next;
    });
  }, [tab]);

  const currentProvider = state?.providers.find((p) => p.isCurrent);
  const liveProviderId = (state?.modelProvider || "openai").trim();
  const liveProviderApiKey = React.useMemo(() => {
    const configKey = extractTomlProviderApiKey(state?.configText, liveProviderId);
    if (!state?.isOfficialProvider) return configKey;
    return extractOpenAiApiKey(state?.authText).trim();
  }, [liveProviderId, state?.authText, state?.configText, state?.isOfficialProvider]);
  const inferredActiveProviderId = React.useMemo(() => {
    if (state?.isOfficialProvider) return "";
    if (state?.activeSavedProviderId && savedProviders.some((item) => item.id === state.activeSavedProviderId)) {
      return state.activeSavedProviderId;
    }
    const fallback = liveProviderId === "custom"
      ? savedProviders.find((item) => item.id === activeProviderId)
      : savedProviders.find((item) => item.id === liveProviderId);
    if (!fallback || !currentProvider) return "";
    return savedProviderMatchesProfile(fallback, {
      baseUrl: currentProvider.baseUrl,
      apiKey: liveProviderApiKey,
      providerName: currentProvider.name,
      model: state?.model,
    }) ? fallback.id : "";
  }, [activeProviderId, currentProvider, liveProviderApiKey, liveProviderId, savedProviders, state?.activeSavedProviderId, state?.isOfficialProvider, state?.model]);
  const effectiveActiveProviderId = state?.isOfficialProvider ? "" : inferredActiveProviderId;
  const currentInstructionPath = (state?.instructionFile || "").replace(/\\/g, "/");
  const currentInstructionFilename = currentInstructionPath.split("/").pop() || "";
  const activeInstructionTitle = React.useMemo(() => {
    const templateKey = state?.instructionTemplateKey || "";
    if (templateKey.startsWith("builtin:")) {
      const id = templateKey.slice("builtin:".length);
      return instructionTemplates.find((item) => item.id === id)?.title || id;
    }
    if (templateKey.startsWith("saved:")) {
      const id = templateKey.slice("saved:".length);
      return savedPrompts.find((item) => item.id === id)?.title || id;
    }
    return savedPrompts.find((item) => item.filename === currentInstructionFilename)?.title
      || instructionTemplates.find((item) => item.filename === currentInstructionFilename)?.title
      || currentInstructionFilename
      || "Current prompt";
  }, [currentInstructionFilename, instructionTemplates, lang, savedPrompts, state?.instructionTemplateKey]);
  const detectedRows = React.useMemo(() => {
    if (state?.isOfficialProvider) return [];
    return (state?.providers || []).filter((p) => p.isCurrent).map((p) => {
      const configKey = extractTomlProviderApiKey(state?.configText, p.id);
      const apiKey = p.isCurrent ? liveProviderApiKey || configKey : configKey;
      return {
        id: `detected-${p.id}`,
        source: "detected" as const,
        providerName: p.name || p.id,
        baseUrl: p.baseUrl || "",
        model: state?.model || "gpt-5.5",
        apiKey,
        wireApi: p.wireApi || "responses",
        requiresOpenaiAuth: p.requiresOpenaiAuth ?? false,
        isCurrent: p.isCurrent,
      };
    });
  }, [liveProviderApiKey, state?.configText, state?.isOfficialProvider, state?.model, state?.providers]);

  const localRows = React.useMemo(() => {
    return savedProviders.map((p) => ({
      ...p,
      source: "local" as const,
      isCurrent: effectiveActiveProviderId === p.id,
    }));
  }, [effectiveActiveProviderId, savedProviders]);

  const currentOfficialProfileId = state?.isOfficialProvider
    ? state.activeOfficialProfileId || DEFAULT_OFFICIAL_PROFILE_ID
    : "";
  const currentOfficialProfile = officialProfiles.find((profile) => profile.id === currentOfficialProfileId);
  const providerRows = React.useMemo<ProviderRow[]>(() => {
    const defaultProfile = officialProfiles.find((profile) => profile.isDefault) || {
      id: DEFAULT_OFFICIAL_PROFILE_ID,
      providerName: "OpenAI Official",
      model: null,
      isDefault: true,
      hasAuth: Boolean(state?.officialAuthAvailable),
      hasOwnedAuth: Boolean(state?.officialAuthAvailable),
      email: null,
      planType: null,
      canQueryQuota: false,
    };
    const officialRow = (profile: typeof defaultProfile): ProviderRow => ({
      id: profile.id,
      source: "official",
      providerName: profile.providerName,
      baseUrl: "https://chatgpt.com/codex",
      model: profile.model || "official",
      apiKey: "",
      wireApi: "official",
      requiresOpenaiAuth: true,
      isCurrent: profile.id === currentOfficialProfileId,
      isDefaultOfficial: profile.isDefault,
      hasAuth: profile.hasOwnedAuth,
      email: profile.email,
      planType: profile.planType,
      canQueryQuota: profile.canQueryQuota,
    });
    const rows = orderProviderRows(officialRow(defaultProfile), detectedRows, localRows);
    return [rows[0], ...officialProfiles.filter((profile) => !profile.isDefault).map(officialRow), ...rows.slice(1)];
  }, [currentOfficialProfileId, detectedRows, lang, localRows, officialProfiles, state?.officialAuthAvailable]);

  const findLocalProviderForRow = React.useCallback((row: ProviderRow) => {
    if (row.source === "official") return undefined;
    if (row.source === "local") return savedProviders.find((item) => item.id === row.id);
    const matches = savedProviders.filter((item) => savedProviderMatchesProfile(item, row));
    return matches.find((item) => item.id === effectiveActiveProviderId)
      || matches.find((item) => item.id === activeProviderId)
      || (matches.length === 1 ? matches[0] : undefined);
  }, [activeProviderId, effectiveActiveProviderId, savedProviders]);

  const providerCopySourceForRow = React.useCallback((row: ProviderRow): SavedProvider | undefined => {
    const local = findLocalProviderForRow(row);
    if (local) return local;
    if (row.source !== "detected") return undefined;
    return {
      id: customProviderId(row.providerName || row.baseUrl),
      providerName: row.providerName,
      baseUrl: row.baseUrl,
      model: row.model,
      apiKey: row.apiKey || "",
      tomlConfig: state?.configText || "",
      wireApi: row.wireApi,
      requiresOpenaiAuth: row.requiresOpenaiAuth,
    };
  }, [findLocalProviderForRow, state?.configText]);

  const providerPageRows = React.useMemo<ProviderRow[]>(() => providerRows.map((row) => {
    const local = findLocalProviderForRow(row);
    return {
      id: row.id,
      source: row.source,
      providerName: row.providerName,
      baseUrl: row.baseUrl,
      model: row.model,
      apiKey: row.apiKey,
      wireApi: row.wireApi,
      requiresOpenaiAuth: row.requiresOpenaiAuth,
      isCurrent: row.isCurrent,
      isDefaultOfficial: row.isDefaultOfficial,
      hasAuth: row.hasAuth,
      email: row.email,
      planType: row.planType,
      canQueryQuota: row.canQueryQuota,
      meta: row.meta,
      sourceLabel: row.source === "official" ? "Codex login" : undefined,
      editable: row.source === "official" || Boolean(local) || row.source === "detected",
      duplicable: row.source === "official" || Boolean(providerCopySourceForRow(row)),
      deletable: row.source === "official" ? !row.isDefaultOfficial : Boolean(local),
      testable: row.source !== "official",
      testingKey: `${row.source}-${row.id}`,
    };
  }), [findLocalProviderForRow, lang, providerCopySourceForRow, providerRows]);

  const visibleSessions = React.useMemo(
    () => (sessionStatus?.sessions || []).filter((item) => showInternalSessions || !item.isSubagent),
    [sessionStatus?.sessions, showInternalSessions],
  );

  const filteredSessions = React.useMemo(() => {
    const query = deferredSessionQuery.trim().toLowerCase();
    if (!query) return visibleSessions;
    return visibleSessions.filter((item) => [item.title, item.cwd, item.rolloutPath, item.modelProvider, item.model, item.id]
      .filter(Boolean)
      .some((value) => String(value).toLowerCase().includes(query)));
  }, [deferredSessionQuery, visibleSessions]);

  const allSessionsByCwd = React.useMemo(() => {
    const groups = new Map<string, SessionPreview[]>();
    for (const item of visibleSessions) {
      const key = item.cwd || "No workspace recorded";
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key)!.push(item);
    }
    return groups;
  }, [lang, visibleSessions]);

  const groupedSessions = React.useMemo(() => {
    const groups = new Map<string, SessionPreview[]>();
    if (!sessionGroupByCwd) {
      groups.set("All sessions", filteredSessions);
      return Array.from(groups.entries());
    }
    for (const item of filteredSessions) {
      const key = item.cwd || "No workspace recorded";
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key)!.push(item);
    }
    return Array.from(groups.entries()).sort((a, b) => b[1].length - a[1].length);
  }, [filteredSessions, lang, sessionGroupByCwd]);

  const sessionHasMismatches = Boolean(sessionStatus?.needsSync);
  const sessionTargetLabel = "Shared history";
  const sessionSyncCount = sessionMismatchCount(sessionStatus);
  const sessionVisibleTotal = showInternalSessions
    ? (sessionStatus?.topLevelThreads ?? 0) + (sessionStatus?.subagentThreads ?? 0)
    : (sessionStatus?.topLevelThreads ?? 0);
  const sessionPreviewTruncated = sessionVisibleTotal > visibleSessions.length;
  const selectedSessionSet = React.useMemo(() => new Set(selectedSessionIds), [selectedSessionIds]);
  const selectedSessions = React.useMemo(
    () => (sessionStatus?.sessions || []).filter((item) => selectedSessionSet.has(item.id)),
    [selectedSessionSet, sessionStatus?.sessions],
  );

  React.useEffect(() => {
    setSelectedSessionIds((ids) => ids.filter((id) => (sessionStatus?.sessions || []).some((item) => item.id === id)));
  }, [sessionStatus?.sessions]);

  React.useEffect(() => {
    if (sessionDeleteConfirmOpen && selectedSessions.length === 0) {
      setSessionDeleteConfirmOpen(false);
    }
  }, [selectedSessions.length, sessionDeleteConfirmOpen]);

  const call = React.useCallback(async <T,>(fn: () => Promise<T>, success?: (data: T) => void) => {
    const loadingToken = beginLoading();
    setError("");
    try {
      const data = await fn();
      success?.(data);
    } catch (e) {
      setError(String(e));
    } finally {
      endLoading(loadingToken);
    }
  }, [beginLoading, endLoading]);

  const refresh = React.useCallback((includeDiagnostics: boolean) => {
    invalidatePromptDetail();
    const requestId = ++refreshRequestRef.current;
    const profilesRequestId = ++officialProfilesRequestRef.current;
    const providersRequestId = ++savedProvidersRequestRef.current;
    officialDraftRequestRef.current += 1;
    clearActionBusy("loadOfficialDraft");
    skillsMcpRequestRef.current += 1;
    skillsMcpLoadedRef.current = "";
    skillsMcpAutoLoadAttemptedRef.current = "";
    const requestedConfigDir = configDirDraft.trim();
    setHealthConfigDir(requestedConfigDir);
    const resolvedConfigDir = requestedConfigDir || null;
    const activeCodexDir = state?.codexDir || configDir;
    setRefreshing(true);
    setError("");
    setState(null);
    setOfficialProfiles([]);
    setSessionStatus(null);
    setSkillsMcpState(null);
    setSkillsMcpImportOpen(false);
    setSkillsMcpImportPreview(null);
    setAboutInfo(null);
    aboutLoadKeyRef.current = "";
    sessionAutoLoadKeyRef.current = "";
    sessionLoadRequestRef.current += 1;

    if (includeDiagnostics) {
      setStartupDiagnostics(null);
      setStartupDiagnosticsError("");
      setStartupDiagnosticsLoading(true);
      void invoke<StartupDiagnostics>("get_startup_diagnostics", { configDir: resolvedConfigDir })
        .then((diagnostics) => {
          if (requestId === refreshRequestRef.current) {
            setStartupDiagnostics(diagnostics);
            setStartupDiagnosticsLoading(false);
          }
        })
        .catch(() => {
          if (requestId === refreshRequestRef.current) {
            setStartupDiagnosticsError("unavailable");
            setStartupDiagnosticsLoading(false);
          }
        });
    } else {
      setStartupDiagnosticsLoading(false);
    }

    void invoke<CodexState>("get_codex_state", { configDir: resolvedConfigDir })
      .then((next) => {
        if (requestId !== refreshRequestRef.current) return;
        if (normalizedConfigDirForComparison(next.codexDir)
          !== normalizedConfigDirForComparison(activeCodexDir)) {
          providerDraftRequestRef.current += 1;
          providerModelsRequestRef.current += 1;
          setProviderMode("list");
          setEditingProviderId(null);
          setEditingOfficialProfileId(null);
          setCreatingProvider(false);
          setEditingDetectedProvider(false);
          setProviderTomlDirty(false);
          setProviderCommonConfigDirty(false);
          setProviderTomlDraft("");
          setAvailableProviderModels([]);
          setProviderModelsLoading(false);
        }
        activeConfigDirKeyRef.current = normalizedConfigDirForComparison(next.codexDir);
        setConfigDir(next.codexDir);
        setHealthConfigDir(next.codexDir);
        setConfigDirDraft(next.codexDir);
        setState(next);
        setRefreshing(false);

        void Promise.allSettled([
          invoke<SavedProvider[]>("list_saved_providers"),
          invoke<SavedPrompt[]>("list_saved_prompts"),
          invoke<BuiltinPromptStatus[]>("get_builtin_prompt_status"),
          invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: next.codexDir }),
        ]).then(([providers, prompts, promptStatus, profiles]) => {
          if (requestId !== refreshRequestRef.current) return;
          if (providersRequestId === savedProvidersRequestRef.current && providers.status === "fulfilled") setSavedProviders(providers.value);
          if (profilesRequestId === officialProfilesRequestRef.current) {
            if (profiles.status === "fulfilled") setOfficialProfiles(profiles.value);
            else setError(String(profiles.reason));
          }
          if (prompts.status === "fulfilled") setSavedPrompts(prompts.value);
          if (promptStatus.status === "fulfilled") {
            setBuiltinPromptStatus(uniqueBuiltinPromptStatuses(promptStatus.value));
          }
        });
      })
      .catch((nextError) => {
        if (requestId !== refreshRequestRef.current) return;
        setRefreshing(false);
        setError(String(nextError));
      });
  }, [clearActionBusy, configDir, configDirDraft, invalidatePromptDetail, state?.codexDir]);

  // Independent of get_codex_state: this must still work when a broken TOML
  // prevents the normal app state from loading. No shared loading flags change.
  const configHealth = useConfigHealth({
    configDir: healthConfigDir,
    canCheck: !refreshing && !loading && !actionBusy
      && !(tab === "provider" && providerMode !== "list") && tab !== "toml",
    canNotify: !toast && !error && !startupWizardOpen && !updatePromptOpen
      && !loading && !refreshing && !actionBusy
      && !(tab === "provider" && providerMode !== "list") && tab !== "toml",
    lang,
    reviewing: startupWizardOpen,
    onHint: setToast,
    onRepaired: () => refresh(startupWizardOpen),
  });

  React.useEffect(() => {
    refresh(startupWizardOpen);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  React.useEffect(() => {
    const directory = configDir;
    const scope = normalizedConfigDirForComparison(directory);
    if (!scope) return;
    let disposed = false;
    let inFlight = false;
    let pending = false;
    let eventRevision = 0;
    let unlisten: (() => void) | undefined;
    const canRead = () => !disposed
      && routingUiRef.current.ready && !routingUiRef.current.editing
      && document.visibilityState !== "hidden"
      && scope === activeConfigDirKeyRef.current
      && loadingTokensRef.current.size === 0 && actionBusyTokensRef.current.size === 0;
    const drain = async () => {
      if (!pending || inFlight || !canRead()) return;
      pending = false;
      inFlight = true;
      const eventGeneration = eventRevision;
      const refreshGeneration = refreshRequestRef.current;
      const loadingGeneration = loadingGenerationRef.current;
      const actionGeneration = actionBusyGenerationRef.current;
      const profileGeneration = officialProfilesRequestRef.current;
      const stillCurrent = () => canRead()
        && refreshGeneration === refreshRequestRef.current
        && loadingGeneration === loadingGenerationRef.current
        && actionGeneration === actionBusyGenerationRef.current
        && eventGeneration === eventRevision;
      try {
        const [next, profiles] = await Promise.allSettled([
          invoke<CodexState>("get_codex_state", { configDir: directory }),
          invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: directory }),
        ]);
        if (!stillCurrent()) {
          pending = !disposed;
          return;
        }
        if (next.status === "fulfilled" && normalizedConfigDirForComparison(next.value.codexDir) === scope) {
          setState((current) => current && stillCurrent()
            && normalizedConfigDirForComparison(current.codexDir) === scope ? next.value : current);
        }
        if (profiles.status === "fulfilled" && profileGeneration === officialProfilesRequestRef.current) {
          officialProfilesRequestRef.current += 1;
          officialProfilesLiveRef.current = profiles.value;
          setOfficialProfiles(profiles.value);
        }
      } catch {
        // A transient bridge failure leaves the current screen and drafts intact.
      } finally {
        inFlight = false;
        // Coalesce events arriving during a read. A busy action/editor will
        // resume this pending update after it closes, preserving unsaved drafts.
        if (pending && canRead()) void drain();
      }
    };
    const wake = () => { void drain(); };
    const request = () => { pending = true; eventRevision += 1; wake(); };
    routingWakeRef.current = wake;
    routingRequestRefreshRef.current = request;
    const onFocus = () => request();
    const onVisibility = () => { if (document.visibilityState !== "hidden") request(); };
    // Backend paths may be canonical while this home is a symlink/alias. Events
    // are hints only: always re-read this configured home, never the payload path.
    void listen<{ codexDir: string }>("provider-routing-changed", () => request())
      .then((release) => { if (disposed) release(); else unlisten = release; }).catch(() => {});
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      pending = false;
      unlisten?.();
      if (routingWakeRef.current === wake) routingWakeRef.current = () => {};
      if (routingRequestRefreshRef.current === request) routingRequestRefreshRef.current = () => {};
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [configDir]);

  React.useEffect(() => {
    routingWakeRef.current();
  }, [loading, actionBusy, refreshing, providerMode]);

  React.useEffect(() => {
    const directory = state?.codexDir;
    if (!directory || !(tab === "dashboard" || (tab === "provider" && providerMode === "list"))) return;
    const scope = normalizedConfigDirForComparison(directory);
    const monitor = createOfficialProfileMonitor({
      read: () => invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: directory }),
      revision: () => officialProfilesRequestRef.current,
      isVisible: () => document.visibilityState !== "hidden",
      canRead: () => officialMonitorReadyRef.current
        && activeConfigDirKeyRef.current === scope
        && loadingTokensRef.current.size === 0
        && actionBusyTokensRef.current.size === 0,
      apply: (profiles) => {
        if (JSON.stringify(profiles) === JSON.stringify(officialProfilesLiveRef.current)) return;
        // This newer snapshot supersedes older foreground list reads, while a
        // mutation that starts after the poll still invalidates it via revision.
        officialProfilesRequestRef.current += 1;
        officialProfilesLiveRef.current = profiles;
        setOfficialProfiles(profiles);
      },
    });
    window.addEventListener("focus", monitor.wake);
    document.addEventListener("visibilitychange", monitor.wake);
    monitor.wake();
    return () => {
      monitor.stop();
      window.removeEventListener("focus", monitor.wake);
      document.removeEventListener("visibilitychange", monitor.wake);
    };
  }, [state?.codexDir, tab, providerMode]);

  React.useEffect(() => {
    if (!state?.codexDir) return;
    const loadKey = state.codexDir;
    if (aboutLoadKeyRef.current === loadKey) return;
    aboutLoadKeyRef.current = loadKey;
    setAboutLoading(true);
    void invoke<AboutInfo>("get_about_info", { configDir: loadKey })
      .then((about) => {
        if (aboutLoadKeyRef.current === loadKey) setAboutInfo(about);
      })
      .catch((aboutError) => {
        if (aboutLoadKeyRef.current !== loadKey) return;
        aboutLoadKeyRef.current = "";
        setError(String(aboutError));
      })
      .finally(() => {
        if (aboutLoadKeyRef.current === loadKey || aboutLoadKeyRef.current === "") {
          setAboutLoading(false);
        }
      });
  }, [state?.codexDir, tab]);

  React.useEffect(() => {
    if (!state) return;
    if (state.isOfficialProvider) {
      if (activeProviderId) {
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
      }
      return;
    }
    if (!savedProviders.length) return;
    const mappedProviderId = savedProviders.some((item) => item.id === inferredActiveProviderId)
      ? inferredActiveProviderId
      : "";
    if (mappedProviderId && mappedProviderId !== activeProviderId) {
      localStorage.setItem(ACTIVE_PROVIDER_KEY, mappedProviderId);
      setActiveProviderId(mappedProviderId);
      return;
    }
    if (activeProviderId && !savedProviders.some((item) => item.id === activeProviderId)) {
      localStorage.removeItem(ACTIVE_PROVIDER_KEY);
      setActiveProviderId("");
    }
  }, [activeProviderId, inferredActiveProviderId, liveProviderId, savedProviders, state]);

  const handleActionResult = (result: ActionResult) => {
    const profilesRequestId = ++officialProfilesRequestRef.current;
    const providersRequestId = ++savedProvidersRequestRef.current;
    setState(result.state);
    setSessionStatus(null);
    sessionAutoLoadKeyRef.current = "";
    sessionLoadRequestRef.current += 1;
    setToast(result.message);
    return Promise.allSettled([
      invoke<SavedPrompt[]>("list_saved_prompts"),
      invoke<SavedProvider[]>("list_saved_providers"),
      invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: result.state.codexDir }),
    ])
      .then(([prompts, providers, profiles]) => {
        if (normalizedConfigDirForComparison(result.state.codexDir) !== activeConfigDirKeyRef.current) return;
        if (prompts.status === "fulfilled") setSavedPrompts(prompts.value);
        if (providersRequestId === savedProvidersRequestRef.current && providers.status === "fulfilled") setSavedProviders(providers.value);
        if (profilesRequestId === officialProfilesRequestRef.current) {
          if (profiles.status === "fulfilled") setOfficialProfiles(profiles.value);
          else setError(String(profiles.reason));
        }
      })
      .catch(() => undefined);
  };

  const switchInstructionTemplate = (templateId: string) =>
    call(
      () => invoke<ActionResult>("enable_instruction_template", { configDir: configDir || null, templateId, injectionMode: promptInjectionMode }),
      handleActionResult,
    );

  const disableInstruction = () =>
    call(
      () => invoke<ActionResult>("disable_instruction", { configDir: configDir || null, deleteFile: true }),
      handleActionResult,
    );

  const disableExternalInstruction = () =>
    call(
      () => invoke<ActionResult>("disable_external_instruction", { configDir: configDir || null }),
      handleActionResult,
    );

  const openAddPrompt = () => {
    invalidatePromptDetail();
    setEditingPromptId(null);
    setEditingBuiltinPrompt(null);
    setPromptForm({ ...blankPromptForm });
    setInstructionMode("form");
  };

  const openEditPrompt = (prompt: SavedPrompt) => {
    invalidatePromptDetail();
    setEditingPromptId(prompt.id);
    setEditingBuiltinPrompt(null);
    setPromptForm(prompt);
    setInstructionMode("form");
  };

  const openEditBuiltinPrompt = async (templateId: string) => {
    const requestId = ++promptDetailRequestRef.current;
    // A local template read must not disable provider or other page actions.
    setPromptDetailLoading(true);
    setError("");
    try {
      const detail = await invoke<BuiltinPromptDetail>("get_builtin_prompt_detail", { templateId });
      if (requestId !== promptDetailRequestRef.current) return;
      setEditingPromptId(null);
      setEditingBuiltinPrompt(detail);
      setPromptForm({
        id: detail.id,
        title: detail.title,
        filename: detail.filename,
        content: detail.content,
      });
      setInstructionMode("form");
    } catch (detailError) {
      if (requestId === promptDetailRequestRef.current) setError(String(detailError));
    } finally {
      if (requestId === promptDetailRequestRef.current) setPromptDetailLoading(false);
    }
  };

  const normalizedPromptForm = (): SavedPrompt => {
    const existing = savedPrompts.filter((item) => item.id !== editingPromptId);
    const requestedFilename = promptForm.filename.trim() || `${providerId(promptForm.title || "prompt")}.md`;
    const filename = editingPromptId ? requestedFilename : uniquePromptFilename(requestedFilename, existing.map((item) => item.filename));
    return {
      ...promptForm,
      id: editingPromptId || uniqueId(promptForm.id || promptForm.title || filename, existing.map((item) => item.id)),
      title: promptForm.title.trim(),
      filename,
      content: promptForm.content,
    };
  };

  const savePromptOnly = () => {
    if (editingBuiltinPrompt) {
      call(
        async () => {
          const detail = await invoke<BuiltinPromptDetail>("save_builtin_prompt_override", {
            templateId: editingBuiltinPrompt.id,
            content: promptForm.content,
          });
          const statuses = await invoke<BuiltinPromptStatus[]>("get_builtin_prompt_status");
          return { detail, statuses };
        },
        ({ detail, statuses }) => {
          setEditingBuiltinPrompt(detail);
          setBuiltinPromptStatus(uniqueBuiltinPromptStatuses(statuses));
          setInstructionMode("list");
          setToast(detail.customized
            ? "Local changes saved for the next activation. Future GitHub syncs will skip this template."
            : "No changes detected. This template will continue to sync from GitHub.");
        },
      );
      return;
    }
    call(
      async () => {
        await invoke<SavedPrompt>("save_prompt", { prompt: normalizedPromptForm() });
        return invoke<SavedPrompt[]>("list_saved_prompts");
      },
      (promptList) => {
        setSavedPrompts(promptList);
        setInstructionMode("list");
        setEditingPromptId(null);
        setToast("Prompt saved");
      },
    );
  };

  const enableSavedPrompt = (id: string) =>
    call(() => invoke<ActionResult>("enable_saved_prompt", { configDir: configDir || null, id, injectionMode: promptInjectionMode }), handleActionResult);

  const removeSavedPrompt = (id: string) =>
    call(
      async () => {
        await invoke<void>("delete_saved_prompt", { id });
        return invoke<SavedPrompt[]>("list_saved_prompts");
      },
      (promptList) => {
        setSavedPrompts(promptList);
        setToast("Prompt deleted");
      },
    );

  const importPromptMd = async (file?: File | null) => {
    if (!file) return;
    if (!file.name.toLowerCase().endsWith(".md")) {
      setError("Please choose a .md prompt file");
      return;
    }
    const actionToken = beginActionBusy("importPrompt");
    const loadingToken = beginLoading();
    setError("");
    try {
      const content = await file.text();
      const title = file.name.replace(/\.md$/i, "");
      const filename = uniquePromptFilename(file.name, savedPrompts.map((item) => item.filename));
      await invoke<SavedPrompt>("save_prompt", {
        prompt: {
          id: uniqueId(title, savedPrompts.map((item) => item.id)),
          title: filename.replace(/\.md$/i, ""),
          filename,
          content,
        },
      });
      const promptList = await invoke<SavedPrompt[]>("list_saved_prompts");
      setSavedPrompts(promptList);
      setToast(`Prompt imported: ${file.name}`);
    } catch (e) {
      setError(String(e));
    } finally {
      endLoading(loadingToken);
      endActionBusy(actionToken);
      if (promptImportRef.current) promptImportRef.current.value = "";
    }
  };

  const refreshBuiltinPrompts = async ({ quiet = false }: { quiet?: boolean } = {}) => {
    const requestId = ++promptRefreshRequestRef.current;
    if (!quiet) promptAutoRefreshAttemptedRef.current = true;
    if (!quiet) setError("");
    try {
      const existingRequest = promptRefreshInFlightRef.current;
      const request = existingRequest || invoke<BuiltinPromptStatus[]>("refresh_builtin_prompts", { configDir: configDir || null });
      if (!existingRequest) {
        promptRefreshInFlightRef.current = request;
        setPromptSyncing(true);
        const clearRequest = () => {
          if (promptRefreshInFlightRef.current !== request) return;
          promptRefreshInFlightRef.current = null;
          setPromptSyncing(false);
        };
        void request.then(clearRequest, clearRequest);
      }
      const list = await request;
      if (requestId !== promptRefreshRequestRef.current) return;
      const uniqueList = uniqueBuiltinPromptStatuses(list);
      const catalogFailed = uniqueList.some((item) => item.syncIssue === "catalog");
      const contentFetchFailures = uniqueList.filter((item) =>
        item.contentSource === "unavailable" || item.syncIssue === "content",
      ).length;
      if (!catalogFailed) {
        promptCatalogReadyRef.current = true;
        setPromptCatalogReady(true);
        setBuiltinPromptStatus(uniqueList);
      } else if (!promptCatalogReadyRef.current) {
        setBuiltinPromptStatus(uniqueList);
      }
      const updated = uniqueList.filter((item) => item.updated).length;
      if (!quiet) {
        setToast(catalogFailed
          ? promptCatalogReadyRef.current
            ? "Online templates are unavailable; keeping the current list"
            : "Online templates are unavailable; using local templates"
          : contentFetchFailures > 0
            ? `Template catalog synced; ${contentFetchFailures} template(s) are using local content`
          : updated > 0
            ? `${updated} prompt template(s) synced`
            : "Prompt templates are up to date");
      }
    } catch (e) {
      if (requestId === promptRefreshRequestRef.current) {
        if (!quiet) setError(String(e));
      }
    }
  };

  const normalizedProviderForm = (tomlConfig = providerTomlDraft || providerForm.tomlConfig || buildProviderTomlPreview(providerForm)): SavedProvider => ({
    ...providerForm,
    id: editingProviderId || uniqueId(providerForm.id || customProviderId(providerForm.providerName || providerForm.baseUrl), savedProviders.map((item) => item.id)),
    providerName: providerForm.providerName.trim(),
    baseUrl: providerForm.baseUrl.trim().replace(/\/+$/, ""),
    model: providerForm.model.trim(),
    apiKey: (providerForm.apiKey || "").trim(),
    tomlConfig: tomlConfig.trimEnd(),
    wireApi: providerForm.wireApi || "responses",
    requiresOpenaiAuth: providerForm.requiresOpenaiAuth,
  });

  const applyProviderConfig = (provider: SavedProvider) => {
    if (savedProviders.some((saved) => saved.id === provider.id)) {
      return invoke<ActionResult>("activate_saved_provider", { configDir: configDir || null, providerId: provider.id });
    }
    const tomlConfig = provider.tomlConfig?.trim();
    if (tomlConfig) {
      return invoke<ActionResult>("save_provider_toml_config", {
        input: {
          configDir: configDir || null,
          configText: tomlConfig,
          apiKey: provider.apiKey || "",
        },
        providerId: provider.id,
      });
    }
    return invoke<ActionResult>("switch_provider", {
      input: {
        configDir: configDir || null,
        providerId: provider.id,
        providerName: provider.providerName,
        baseUrl: provider.baseUrl,
        model: provider.model,
        apiKey: provider.apiKey || "",
        wireApi: provider.wireApi,
        requiresOpenaiAuth: provider.requiresOpenaiAuth,
      },
    });
  };

  const saveProviderOnly = () => {
    const pendingProvider = normalizedProviderForm();
    const mappingValidation = validateProviderModelMappings(pendingProvider.modelMappings || [], pendingProvider.model, lang);
    if (pendingProvider.modelMappings?.length && pendingProvider.wireApi !== "responses") {
      setError("Model mappings require the Responses API. Set Wire API to responses");
      return;
    }
    if (!mappingValidation.valid) {
      setError("Correct the model mappings before saving");
      return;
    }
    if (!pendingProvider.providerName || !pendingProvider.baseUrl || !pendingProvider.model) {
      setError("Provider name, API URL, and model are required");
      return;
    }
    return call(
      async () => {
        let provider = pendingProvider;
        if (!providerTomlDirty) {
          const draftSource = providerForm.tomlConfig?.trim() || "";
          const providerForDraft = normalizedProviderForm(draftSource);
          const latestDraft = await invoke<string>("build_provider_toml_draft", {
            provider: providerForDraft,
            newProvider: creatingProvider,
            configDir: configDir || null,
          });
          provider = { ...providerForDraft, tomlConfig: latestDraft.trimEnd() };
          setProviderTomlDraft(provider.tomlConfig || "");
        }
        const applyAfterSave = editingDetectedProvider
          || Boolean(editingProviderId && editingProviderId === effectiveActiveProviderId);
        const applied = applyAfterSave
          ? await invoke<ActionResult>("save_active_provider", { provider, configDir: configDir || null, applyCommonConfig: providerCommonConfigDirty })
          : null;
        if (!applyAfterSave) await invoke<SavedProvider>("save_provider", { provider });
        const providerList = await invoke<SavedProvider[]>("list_saved_providers");
        return { applied, providerList };
      },
      ({ applied, providerList }) => {
        if (applied) handleActionResult(applied);
        commitSavedProviders(providerList);
        setProviderMode("list");
        setEditingProviderId(null);
        setEditingDetectedProvider(false);
        setProviderTomlDirty(false);
        setProviderCommonConfigDirty(false);
        setToast(applied && pendingProvider.modelMappings?.length
          ? "Saved. Restart Codex to update its model menu"
          : applied
          ? "Provider saved and hot-applied"
          : "Provider saved");
      },
    );
  };

  const switchProvider = (provider: SavedProvider) =>
    call(
      () => applyProviderConfig(provider),
      (result) => {
        localStorage.setItem(ACTIVE_PROVIDER_KEY, provider.id);
        setActiveProviderId(provider.id);
        handleActionResult(result);
        if (provider.modelMappings?.length) {
          setToast("Enabled. Restart Codex to update its model menu");
        }
      },
    );

  const resetAvailableProviderModels = () => {
    providerModelsRequestRef.current += 1;
    setAvailableProviderModels([]);
    setProviderModelsLoading(false);
  };

  const fetchProviderModels = async () => {
    const baseUrl = providerForm.baseUrl.trim();
    const apiKey = (providerForm.apiKey || "").trim();
    if (!baseUrl || !apiKey) {
      setError("");
      setToast("Enter the API URL and API key first");
      return;
    }

    const requestId = providerModelsRequestRef.current + 1;
    providerModelsRequestRef.current = requestId;
    setProviderModelsLoading(true);
    setError("");
    setToast("Fetching model list...");
    try {
      const result = await invoke<ProviderModelsResult>("fetch_provider_models", { baseUrl, apiKey });
      if (providerModelsRequestRef.current !== requestId) return;
      setAvailableProviderModels(result.models);
      setToast(result.models.length > 0
        ? `${result.models.length} models fetched`
        : "Connected, but the provider returned no models");
    } catch (e) {
      if (providerModelsRequestRef.current !== requestId) return;
      setToast("");
      setError(String(e));
    } finally {
      if (providerModelsRequestRef.current === requestId) setProviderModelsLoading(false);
    }
  };

  const testProvider = async (id: string, baseUrl: string, apiKey?: string | null) => {
    const actionToken = beginActionBusy("testProvider");
    setProviderTestingId(id);
    setError("");
    setToast("Testing connection...");
    try {
      const result = await invoke<ProviderConnectionResult>("test_provider_connection", { baseUrl, apiKey: apiKey || null });
      if (result.ok) {
        setToast(`Connected, ${result.durationMs}ms latency`);
      } else {
        setToast("");
        setError(`Connection failed: ${result.message}`);
      }
    } catch (e) {
      setToast("");
      setError(String(e));
    } finally {
      setProviderTestingId("");
      endActionBusy(actionToken);
    }
  };

  const saveProviderConfig = saveProviderOnly;

  const switchOfficialProvider = (profileId = DEFAULT_OFFICIAL_PROFILE_ID) =>
    call(
      () => invoke<ActionResult>("switch_official_profile", { configDir: configDir || null, profileId }),
      (result) => {
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
        handleActionResult(result);
      },
    );

  const loadCcSwitchOfficial = async () => {
    const requestId = ++officialDraftRequestRef.current;
    const actionToken = beginActionBusy("loadCcSwitchOfficial");
    setError("");
    try {
      const candidate = await invoke<OfficialAuthCandidate | null>("read_ccswitch_official_auth", {
        dbPath: null,
      });
      if (requestId !== officialDraftRequestRef.current) return;
      if (!candidate) {
        setToast("No CC Switch official config found");
        return;
      }
      setOfficialAuthDirty(true);
      setOfficialForm((current) => ({
        ...current,
        model: candidate.model || current.model || state?.model || "gpt-5.5",
        authJson: candidate.authJson,
        configText: candidate.configText || current.configText,
      }));
      setToast("Loaded from CC Switch");
    } catch (e) {
      if (requestId === officialDraftRequestRef.current) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const resetOfficialProvider = () =>
    call(
      () => invoke<ActionResult>("reset_official_provider", {
        input: {
          configDir: configDir || null,
          model: officialForm.model,
          authJson: null,
          configText: officialForm.configText,
        },
      }),
      (result) => {
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
        setOfficialAuthDirty(false);
        setOfficialForm({
          providerName: officialForm.providerName,
          model: result.state.model || officialForm.model || "gpt-5.5",
          authJson: "",
          configText: result.state.configText
            || officialForm.configText
            || buildOfficialTomlPreview(result.state.model || officialForm.model || "gpt-5.5"),
        });
        handleActionResult(result);
      },
    );

  const importFromCcSwitch = async () => {
    const actionToken = beginActionBusy("importCcSwitch");
    const loadingToken = beginLoading();
    setError("");
    try {
      const result = await invoke<ImportResult>("import_ccswitch_codex_providers", { dbPath: null });
      const warningText = result.skipped > 0
        ? `, ${result.skipped} skipped`
        : "";
      const successText = `cc-switch import complete: ${result.added} added, ${result.updated} updated, ${result.merged} merged${warningText}; current provider unchanged`;
      try {
        const nextState = await invoke<CodexState>("get_codex_state", { configDir: configDir || null });
        commitSavedProviders(result.providers);
        setState(nextState);
        setToast(successText);
      } catch (refreshError) {
        commitSavedProviders(result.providers);
        setToast(`${successText}; state refresh failed, refresh manually: ${String(refreshError)}`);
      }
    } catch (importError) {
      setError(String(importError));
    } finally {
      endLoading(loadingToken);
      endActionBusy(actionToken);
    }
  };

  const openExternalUrl = React.useCallback((url?: string | null) => {
    if (!url) return;
    window.setTimeout(() => {
      void invoke("open_url", { url }).catch(() => {
        setToast("Failed to open browser");
      });
    }, 0);
  }, [lang]);

  const checkForUpdates = React.useCallback(async ({ quiet = false }: { quiet?: boolean } = {}) => {
    setReleaseInfo({ status: "checking" });
    try {
      if (aboutInfo?.nativeUpdaterSupported !== false) {
        const updaterResult = await appUpdater.check({ force: !quiet, timeout: 15_000 });
        if (updaterResult === "available") {
          const snapshot = appUpdater.getSnapshot();
          const latestVersion = snapshot.latestVersion || "";
          const releaseTag = latestVersion.startsWith("v") ? latestVersion : `v${latestVersion}`;
          setReleaseInfo({
            status: "ok",
            latestVersion: releaseTag,
            htmlUrl: `https://github.com/${FALLBACK_GITHUB_REPO}/releases/tag/${releaseTag}`,
            hasUpdate: true,
            updateMethod: "native",
          });
          if (quiet) {
            setToast(`New version ${releaseTag} is available`);
          } else {
            setUpdatePromptOpen(true);
          }
          return;
        }

        if (updaterResult === "up-to-date") {
          setReleaseInfo({
            status: "ok",
            latestVersion: aboutInfo?.appVersion,
            htmlUrl: `https://github.com/${FALLBACK_GITHUB_REPO}/releases/latest`,
            hasUpdate: false,
          });
          if (!quiet) setToast("You are up to date");
          return;
        }
      }

      // Keep the existing lightweight release check as a manual-download fallback for
      // bootstrap and portable builds that cannot use the native updater yet.
      const update = await invoke<AppUpdateInfo>("check_app_update");
      const message = update.hasUpdate
        ? "Update available"
        : "You are up to date";
      setReleaseInfo({
        status: "ok",
        latestVersion: update.latestVersion,
        htmlUrl: update.htmlUrl,
        hasUpdate: update.hasUpdate,
        updateMethod: update.hasUpdate ? "download" : undefined,
      });
      if (update.hasUpdate) {
        if (quiet) {
          setToast(`New version ${update.latestVersion} is available`);
        } else {
          setUpdatePromptOpen(true);
        }
      } else if (!quiet) {
        setToast(message);
      }
    } catch {
      const message = quiet ? "Auto check failed" : "Check failed";
      setReleaseInfo({
        status: "error",
      });
      if (!quiet) setToast(message);
    }
  }, [aboutInfo?.appVersion, aboutInfo?.nativeUpdaterSupported, lang]);

  React.useEffect(() => {
    if (!state || !aboutInfo || autoUpdateCheckedRef.current) return;
    autoUpdateCheckedRef.current = true;
    void checkForUpdates({ quiet: true });
  }, [aboutInfo, state, checkForUpdates]);

  React.useEffect(() => {
    if (!state || tab !== "instruction" || promptAutoRefreshAttemptedRef.current) return;
    promptAutoRefreshAttemptedRef.current = true;
    void refreshBuiltinPrompts({ quiet: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state, tab]);

  const beginSkillsMcpRequest = React.useCallback(() => ({
    requestId: ++skillsMcpRequestRef.current,
    configDir: configDir || null,
    configDirKey: normalizedConfigDirForComparison(configDir),
  }), [configDir]);

  const isCurrentSkillsMcpRequest = React.useCallback((requestId: number, configDirKey: string) => (
    requestId === skillsMcpRequestRef.current
      && configDirKey === activeConfigDirKeyRef.current
  ), []);

  const loadSkillsMcp = React.useCallback(async ({ quiet = false }: { quiet?: boolean } = {}) => {
    const request = beginSkillsMcpRequest();
    const actionToken = quiet ? null : beginActionBusy("loadSkillsMcp");
    if (!quiet) setError("");
    try {
      const result = await invoke<SkillsMcpState>("get_skills_mcp_state", { configDir: request.configDir });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
    } catch (e) {
      if (!quiet && isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) {
        setError(String(e));
      }
    } finally {
      if (actionToken !== null) endActionBusy(actionToken);
    }
  }, [beginActionBusy, beginSkillsMcpRequest, endActionBusy, isCurrentSkillsMcpRequest]);

  React.useEffect(() => {
    const configDirKey = normalizedConfigDirForComparison(configDir);
    if (tab !== "skillsMcp") {
      skillsMcpAutoLoadAttemptedRef.current = "";
      return;
    }
    if (!state?.codexDir
      || !configDirKey
      || Boolean(actionBusy)
      || skillsMcpAutoLoadAttemptedRef.current === configDirKey
      || skillsMcpLoadedRef.current === configDirKey) return;
    skillsMcpAutoLoadAttemptedRef.current = configDirKey;
    void loadSkillsMcp();
  }, [actionBusy, configDir, loadSkillsMcp, state?.codexDir, tab]);

  const openImportExistingSkillsMcpPreview = async () => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("previewExistingSkillsMcp");
    setError("");
    try {
      const preview = await invoke<SkillsMcpImportPreview>("preview_existing_skills_mcp", { configDir: request.configDir });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      if (preview.skills.length + preview.mcpServers.length === 0) {
        setSkillsMcpImportPreview(null);
        setSkillsMcpImportOpen(false);
        setToast("No new Skills or MCP items to import");
        return;
      }
      setSkillsMcpImportPreview(preview);
      setSkillsMcpImportOpen(true);
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const importExistingSkillsMcp = async () => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("importExistingSkillsMcp");
    setError("");
    try {
      const result = await invoke<SkillsMcpActionResult>("import_existing_skills_mcp", { configDir: request.configDir });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result.state);
      setSkillsMcpImportOpen(false);
      setSkillsMcpImportPreview(null);
      setToast(result.message);
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const checkSkillUpdatesAction = async () => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("checkSkillUpdates");
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("check_skill_updates", { configDir: request.configDir });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
      setToast("Skill update status refreshed");
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const toggleSkillEnabled = async (id: string, enabled: boolean) => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy(`skill:${id}`);
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("toggle_codex_skill", { configDir: request.configDir, id, enabled });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
      setToast(enabled ? "Skill enabled" : "Skill disabled");
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const toggleMcpEnabled = async (id: string, enabled: boolean) => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy(`mcp:${id}`);
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("toggle_codex_mcp", { configDir: request.configDir, id, enabled });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
      setToast(enabled ? "MCP enabled" : "MCP disabled");
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const saveSkillsMcpNote = async (itemKind: SkillsMcpNoteKind, id: string, note: string) => {
    if (skillsMcpNoteBusyRef.current) return false;
    const noteBusyKey = `note:${itemKind}:${id}`;
    skillsMcpNoteBusyRef.current = noteBusyKey;
    setSkillsMcpNoteBusy(noteBusyKey);
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy(noteBusyKey);
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("save_skills_mcp_note", {
        configDir: request.configDir,
        itemKind,
        id,
        note,
      });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return false;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
      setToast("Note saved");
      return true;
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
      return false;
    } finally {
      endActionBusy(actionToken);
      if (skillsMcpNoteBusyRef.current === noteBusyKey) {
        skillsMcpNoteBusyRef.current = "";
        setSkillsMcpNoteBusy("");
      }
    }
  };

  const restartCodexDesktop = async () => {
    if (restartCodexBusyRef.current) return false;
    restartCodexBusyRef.current = true;
    setRestartCodexBusy(true);
    const actionToken = beginActionBusy("restartCodexDesktop");
    setError("");
    try {
      const result = await invoke<CodexDesktopRestartResult>("restart_codex_desktop");
      setToast(result.wasRunning
        ? `${result.appName} restarted`
        : `${result.appName} started`);
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    } finally {
      endActionBusy(actionToken);
      restartCodexBusyRef.current = false;
      setRestartCodexBusy(false);
    }
  };

  const installSkillZipFile = async () => {
    if (nativeTransferBusyRef.current) return;
    nativeTransferBusyRef.current = true;
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("installSkillZip");
    setError("");
    try {
      const result = await invoke<SkillsMcpActionResult | null>("import_skills_mcp_archive", { configDir: request.configDir, lang });
      if (!result || !isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result.state);
      setToast(result.message);
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
      nativeTransferBusyRef.current = false;
    }
  };

  const exportSkillsMcp = async (kind: "mcp" | "skills") => {
    if (nativeTransferBusyRef.current) return;
    nativeTransferBusyRef.current = true;
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("exportSkillsMcp");
    setError("");
    try {
      const result = await invoke<{ path: string; exportedSkills: number; exportedMcp: number } | null>("export_skills_mcp_archive", { configDir: request.configDir, kind, lang });
      if (!result || !isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      const count = kind === "mcp" ? result.exportedMcp : result.exportedSkills;
      setToast(`Exported ${count} ${kind === "mcp" ? "MCP servers" : "Skills"}`);
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
      nativeTransferBusyRef.current = false;
    }
  };

  const exportSessions = async (ids: string[]) => {
    if (!ids.length || nativeTransferBusyRef.current) return;
    nativeTransferBusyRef.current = true;
    setSessionExportBusy(true);
    const directory = configDir;
    const directoryKey = normalizedConfigDirForComparison(directory);
    const generation = refreshRequestRef.current;
    const isCurrentExport = () => generation === refreshRequestRef.current && activeConfigDirKeyRef.current === directoryKey;
    const actionToken = beginActionBusy("exportSessions");
    setError("");
    try {
      const suggestedName = ids.length === 1
        ? sessionStatus?.sessions.find((session) => session.id === ids[0])?.title || "Codex-session"
        : `Codex-sessions-${new Date().toISOString().slice(0, 10)}`;
      const result = await invoke<{ path: string; exportedSessions: number; failedSessions: number; warnings: string[] } | null>("export_codex_sessions", { configDir: directory || null, sessionIds: ids, suggestedName, lang });
      if (!result || !isCurrentExport()) return;
      const message = `Exported ${result.exportedSessions} conversation(s)`;
      if (result.failedSessions || result.warnings.length) setError(`${message}；${result.warnings.join("；")}`);
      else setToast(message);
    } catch (e) {
      if (isCurrentExport()) setError(String(e));
    } finally {
      endActionBusy(actionToken);
      setSessionExportBusy(false);
      nativeTransferBusyRef.current = false;
    }
  };

  const openOfficialEdit = async (profileId = DEFAULT_OFFICIAL_PROFILE_ID) => {
    providerCreationRequestRef.current += 1;
    const requestId = ++officialDraftRequestRef.current;
    const actionToken = beginActionBusy("loadOfficialDraft");
    const profile = officialProfiles.find((item) => item.id === profileId);
    setEditingDetectedProvider(false);
    setEditingOfficialProfileId(profileId);
    setCreatingProvider(false);
    setOfficialAuthDirty(false);
    setOfficialForm({
      providerName: profile?.providerName || "OpenAI Official",
      model: profile?.model || "gpt-5.5",
      authJson: "",
      configText: "",
    });
    setProviderMode("official");
    setError("");
    try {
      const draft = await invoke<OfficialProfileDetail>("get_official_profile", {
        configDir: configDir || null,
        profileId,
      });
      if (requestId !== officialDraftRequestRef.current) return;
      setOfficialForm({
        providerName: draft.providerName,
        model: draft.model || "gpt-5.5",
        authJson: draft.authJson,
        configText: draft.configText,
      });
    } catch (e) {
      if (requestId === officialDraftRequestRef.current) {
        setError(String(e));
        setProviderMode("list");
      }
    } finally {
      endActionBusy(actionToken);
    }
  };

  const saveOfficialConfig = () => {
    if (!officialForm.providerName.trim()) {
      setError("Provider name is required");
      return;
    }
    return call(
      () =>
        invoke<OfficialProfileActionResult>("save_official_profile", {
          input: {
            configDir: configDir || null,
            id: editingOfficialProfileId,
            providerName: officialForm.providerName.trim(),
            model: officialForm.model,
            authJson: officialAuthDirty || editingOfficialProfileId === null ? officialForm.authJson : null,
            configText: officialForm.configText,
          },
        }),
      (result) => {
        handleActionResult(result);
        setCreatingProvider(false);
        setProviderMode("list");
      },
    );
  };

  const loadCurrentOfficial = async () => {
    const requestId = ++officialDraftRequestRef.current;
    const actionToken = beginActionBusy("loadCurrentOfficial");
    setError("");
    try {
      const current = await invoke<CodexState>("get_codex_state", { configDir: configDir || null });
      if (requestId !== officialDraftRequestRef.current) return;
      if (!current.isOfficialProvider) {
        throw new Error("Switch Codex to official sign-in first");
      }
      setOfficialAuthDirty(true);
      setOfficialForm((form) => ({
        ...form,
        model: current.model || form.model,
        configText: current.configText,
        authJson: current.authText || "",
      }));
      setToast("Current sign-in loaded. Save to keep it in this profile.");
    } catch (loadError) {
      if (requestId === officialDraftRequestRef.current) setError(String(loadError));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const duplicateOfficialProfile = (row: ProviderRow) => {
    const providerName = `${row.providerName}${" Copy"}`;
    return call(
      () => invoke<OfficialProfileActionResult>("duplicate_official_profile", {
        configDir: configDir || null, profileId: row.id, providerName,
      }),
      (result) => {
        if (normalizedConfigDirForComparison(result.state.codexDir) !== activeConfigDirKeyRef.current) return;
        handleActionResult(result);
        setToast(`Created “${result.profile.providerName}”`);
      },
    );
  };

  const changeProviderKind = async (kind: "api" | "official") => {
    const requestId = ++officialDraftRequestRef.current;
    clearActionBusy("loadOfficialDraft");
    if (kind === "api") {
      setProviderMode("form");
      return;
    }
    setEditingOfficialProfileId(null);
    setProviderMode("official");
    if (officialForm.configText.trim()) return;
    // Start with a complete official template so a context-window toggle does
    // not turn an otherwise empty draft into a two-line replacement config.
    const actionToken = beginActionBusy("loadOfficialDraft");
    try {
      const template = await invoke<OfficialProfileDetail>("get_official_profile", {
        configDir: configDir || null, profileId: DEFAULT_OFFICIAL_PROFILE_ID,
      });
      if (requestId !== officialDraftRequestRef.current) return;
      setOfficialForm((form) => ({ ...form, configText: template.configText, model: template.model || form.model }));
    } catch (templateError) {
      if (requestId === officialDraftRequestRef.current) {
        setError(String(templateError));
        setProviderMode("list");
      }
    } finally {
      endActionBusy(actionToken);
    }
  };

  const newCustomProviderForm = (configText = providerCreationBase): SavedProvider => ({
    ...blankProviderForm,
    model: state?.model?.trim() || blankProviderForm.model,
    wireApi: currentProvider?.wireApi?.trim() || blankProviderForm.wireApi,
    requiresOpenaiAuth: currentProvider?.requiresOpenaiAuth ?? blankProviderForm.requiresOpenaiAuth,
    tomlConfig: configText.trim(),
  });

  const openAddProvider = () => {
    const requestId = ++providerCreationRequestRef.current;
    const requestedDir = activeConfigDirKeyRef.current;
    const contextGeneration = refreshRequestRef.current;
    // Read a fresh, shared configuration once for this creation flow. The UI's
    // last state may be stale or a legacy provider-only template.
    return call(
      () => invoke<string>("get_provider_config_base", { configDir: configDir || null }),
      (configText) => {
        if (requestId !== providerCreationRequestRef.current || requestedDir !== activeConfigDirKeyRef.current
          || contextGeneration !== refreshRequestRef.current) return;
        officialDraftRequestRef.current += 1;
        setProviderCreationBase(configText);
        setCreatingProvider(true);
        setSelectedPresetId("custom");
        setSelectedPresetVariantId("");
        setEditingOfficialProfileId(null);
        setOfficialAuthDirty(false);
        setOfficialForm({ providerName: "", model: state?.model || "gpt-5.5", configText: "", authJson: "" });
        const next = newCustomProviderForm(configText);
        resetAvailableProviderModels();
        setEditingProviderId(null);
        setEditingDetectedProvider(false);
        setProviderForm(next);
        setProviderTomlDraft(next.tomlConfig || buildProviderTomlPreview(next));
        setProviderTomlDirty(false);
        setProviderCommonConfigDirty(false);
        setProviderMode("form");
      },
    );
  };

  const applyProviderPreset = (presetId: string, variantId: string) => {
    const preset = getProviderPreset(presetId);
    if (!creatingProvider || !preset) return;
    const variant = getProviderPresetVariant(presetId, variantId);
    setSelectedPresetId(presetId);
    setSelectedPresetVariantId(variant?.id || "");
    resetAvailableProviderModels();
    setError("");
    if (presetId === "official") {
      void changeProviderKind("official");
      return;
    }
    officialDraftRequestRef.current += 1;
    providerDraftRequestRef.current += 1;
    clearActionBusy("loadOfficialDraft");
    const inherited = newCustomProviderForm();
    const draft = createPresetProvider(presetId, variantId, inherited.tomlConfig) || inherited;
    // Presets use the same full TOML base as Custom. The existing draft builder
    // updates provider fields while retaining common and extended settings.
    // Switching services still starts with an empty API key field.
    const next = {
      ...draft,
      id: draft.id ? uniqueId(draft.id, savedProviders.map((provider) => provider.id)) : "",
    };
    setProviderForm(next);
    setProviderTomlDraft(next.tomlConfig || buildProviderTomlPreview(next));
    setProviderTomlDirty(false);
    setProviderCommonConfigDirty(false);
    setProviderApiKeyVisible(false);
    setAvailableProviderModels(variant?.models.map((entry) => ({ id: entry.model })) || []);
    setProviderMode("form");
  };

  const openEditProvider = (provider: SavedProvider) => {
    providerCreationRequestRef.current += 1;
    setCreatingProvider(false);
    resetAvailableProviderModels();
    setEditingProviderId(provider.id);
    setEditingDetectedProvider(false);
    setProviderForm(provider);
    setProviderTomlDraft(provider.tomlConfig?.trim() || buildProviderTomlPreview(provider));
    setProviderTomlDirty(false);
    setProviderCommonConfigDirty(false);
    setProviderMode("form");
  };

  const duplicateProvider = (row: ProviderRow) => {
    const requestedDirKey = activeConfigDirKeyRef.current;
    return call(
      () => invoke<DuplicateProviderResult>("duplicate_provider", {
        configDir: configDir || null,
        providerId: findLocalProviderForRow(row)?.id || null,
        providerName: `${row.providerName}${" Copy"}`,
      }),
      (result) => {
        if (requestedDirKey !== activeConfigDirKeyRef.current) return;
        commitSavedProviders(result.providers);
        if (result.activeProviderId) {
          setActiveProviderId(result.activeProviderId);
          localStorage.setItem(ACTIVE_PROVIDER_KEY, result.activeProviderId);
          setState((current) => current && !current.isOfficialProvider
            ? { ...current, activeSavedProviderId: result.activeProviderId || undefined } : current);
        }
        setToast(`Created “${result.provider.providerName}”`);
      },
    );
  };

  const openEditDetectedProvider = (provider: { id: string; providerName: string; baseUrl: string; model: string; apiKey?: string; wireApi: string; requiresOpenaiAuth: boolean }) => {
    providerCreationRequestRef.current += 1;
    setCreatingProvider(false);
    resetAvailableProviderModels();
    const id = uniqueId(
      customProviderId(provider.providerName || provider.baseUrl),
      savedProviders.map((item) => item.id),
    );
    setEditingProviderId(id);
    setEditingDetectedProvider(true);
    const next = {
      id,
      providerName: provider.providerName,
      baseUrl: provider.baseUrl,
      model: provider.model,
      apiKey: provider.apiKey || "",
      tomlConfig: state?.configText?.trim() || "",
      wireApi: provider.wireApi || "responses",
      requiresOpenaiAuth: provider.requiresOpenaiAuth,
    };
    setProviderForm(next);
    setProviderTomlDraft(next.tomlConfig || buildProviderTomlPreview(next));
    setProviderTomlDirty(false);
    setProviderCommonConfigDirty(false);
    setProviderMode("form");
  };

  const removeProvider = async (id: string, isCurrent: boolean) => {
    const loadingToken = beginLoading();
    setError("");
    try {
      if (isCurrent) {
        const result = await invoke<ActionResult>("switch_official_provider", { configDir: configDir || null });
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
        setState(result.state);
      }
      await invoke<void>("delete_saved_provider", { id, configDir: configDir || null });
      const providerList = await invoke<SavedProvider[]>("list_saved_providers");
      commitSavedProviders(providerList);
      setToast("Provider deleted");
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    } finally {
      endLoading(loadingToken);
    }
  };

  const removeOfficialProfile = async (id: string, isCurrent: boolean) => {
    const loadingToken = beginLoading();
    setError("");
    try {
      if (isCurrent) {
        const result = await invoke<ActionResult>("switch_official_profile", {
          configDir: configDir || null, profileId: DEFAULT_OFFICIAL_PROFILE_ID,
        });
        await handleActionResult(result);
      }
      await invoke<void>("delete_official_profile", { configDir: configDir || null, profileId: id });
      const profilesRequestId = ++officialProfilesRequestRef.current;
      const profiles = await invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: configDir || null });
      if (profilesRequestId === officialProfilesRequestRef.current) setOfficialProfiles(profiles);
      setToast("Official sign-in profile deleted");
      return true;
    } catch (deleteError) {
      setError(String(deleteError));
      return false;
    } finally {
      endLoading(loadingToken);
    }
  };

  const checkSessions = async () => {
    sessionLoadRequestRef.current += 1;
    const actionToken = beginActionBusy("checkSessions");
    setSessionStatus(null);
    await call(
      () => invoke<SessionSyncStatus>("get_session_sync_status", { configDir: configDir || null, targetProvider: null }),
      (status) => {
        setSessionStatus(status);
        if (!status.scanComplete) {
          setToast(status.scanFailures[0] || "Unable to verify session sync status");
          return;
        }
        const hasMismatches = Boolean(status.needsSync);
        const syncCount = sessionMismatchCount(status);
        setToast(hasMismatches
          ? `${syncCount} session(s) need syncing`
          : "User conversations are synced");
      },
    );
    endActionBusy(actionToken);
  };

  React.useEffect(() => {
    if (tab !== "sessions" || refreshing || !state?.codexDir) return;
    const loadKey = state.codexDir;
    if (sessionAutoLoadKeyRef.current === loadKey) return;
    const requestId = ++sessionLoadRequestRef.current;
    const actionToken = beginActionBusy("checkSessions");
    sessionAutoLoadKeyRef.current = loadKey;
    void invoke<SessionSyncStatus>("get_session_sync_status", {
      configDir: loadKey,
      targetProvider: null,
    })
      .then((status) => {
        if (requestId === sessionLoadRequestRef.current) setSessionStatus(status);
      })
      .catch((sessionError) => {
        if (requestId !== sessionLoadRequestRef.current) return;
        sessionAutoLoadKeyRef.current = "";
        setError(String(sessionError));
      })
      .finally(() => {
        endActionBusy(actionToken);
      });
  }, [beginActionBusy, endActionBusy, refreshing, state?.codexDir, tab]);

  const syncSessions = async () => {
    sessionLoadRequestRef.current += 1;
    const actionToken = beginActionBusy("syncSessions");
    await call(
      () => invoke<SessionSyncResult>("sync_sessions_provider", { configDir: configDir || null, targetProvider: null }),
      (result) => {
        setSessionStatus(result.status);
        setSelectedSessionIds([]);
        if (!result.status.scanComplete) {
          setToast(result.status.scanFailures[0] || "Unable to verify session sync status");
        } else if (result.status.needsSync) {
          const remaining = sessionMismatchCount(result.status);
          setToast(`Writable session indexes were synced; ${remaining} still need retrying. Chat content was not changed.`);
        } else {
          setToast("All session indexes are synced. Chat content was not changed.");
        }
      },
    );
    endActionBusy(actionToken);
  };

  const toggleSessionSelected = (id: string) => {
    setSelectedSessionIds((ids) => ids.includes(id) ? ids.filter((item) => item !== id) : [...ids, id]);
  };

  const setSessionGroupSelected = (sessions: SessionPreview[], checked: boolean) => {
    const groupIds = new Set(sessions.map((item) => item.id));
    setSelectedSessionIds((ids) => {
      const next = new Set(ids);
      if (checked) groupIds.forEach((id) => next.add(id));
      else groupIds.forEach((id) => next.delete(id));
      return Array.from(next);
    });
  };

  const closeSessionDeleteConfirm = () => {
    if (!sessionDeleteBusy) {
      setSessionDeleteConfirmOpen(false);
      setSessionDeleteSafetyConfirmed(false);
    }
  };

  const deleteSelectedSessions = async () => {
    if (!selectedSessionIds.length || sessionDeleteBusy || !sessionDeleteSafetyConfirmed) return;
    setSessionDeleteBusy(true);
    setToast("");
    setError("");
    try {
      const result = await invoke<SessionDeleteResult>("delete_codex_sessions", {
        input: {
          configDir: configDir || null,
          sessionIds: selectedSessionIds,
        },
      });
      setSessionStatus(result.status);
      const remainingIds = new Set(result.status.sessions.map((item) => item.id));
      setSelectedSessionIds((ids) => ids.filter((id) => remainingIds.has(id)));
      setSessionDeleteConfirmOpen(false);
      setSessionDeleteSafetyConfirmed(false);
      const hasPartialFailure = result.failedSessions > 0 || Boolean(result.failureMessage);
      if (hasPartialFailure) {
        setError(result.failureMessage || `${result.failedSessions} session deletion(s) failed. Close other Codex windows or CLIs and retry.`);
      } else {
        setToast(`Permanently deleted ${result.deletedSessions} session(s) and cleaned database, rollout, and related history data`);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setSessionDeleteBusy(false);
    }
  };

  const closeStartupWizard = () => {
    if (startupCheckMode === "startup") localStorage.setItem(STARTUP_WIZARD_SEEN_KEY, "1");
    setStartupClosing(true);
    window.setTimeout(() => {
      setStartupWizardOpen(false);
      setStartupClosing(false);
    }, 260);
  };

  const changeTab = (nextTab: Tab) => {
    if (nextTab !== "instruction") invalidatePromptDetail();
    if (nextTab !== "provider") {
      officialDraftRequestRef.current += 1;
      if (actionBusy === "loadOfficialDraft") setProviderMode("list");
      clearActionBusy("loadOfficialDraft");
    }
    setTab(nextTab);
  };

  const openConfigurationChecks = () => {
    configHealth.dismiss();
    setStartupCheckMode("manual");
    setStartupClosing(false);
    setStartupWizardOpen(true);
    refresh(true);
  };

  return (
    <AppShell
      activeTab={tab}
      onTabChange={changeTab}
      lang={lang}
      theme={theme}
      onToggleTheme={toggleTheme}
      codexVersion={aboutInfo?.codexVersion
        || (aboutLoading ? "Detecting..." : undefined)}
      appVersion={aboutInfo?.appVersion}
      hasUpdate={Boolean(releaseInfo.hasUpdate)}
      updatePhase={updater.state.phase}
      onOpenUpdate={() => setUpdatePromptOpen(true)}
      isMacRuntime={isMacRuntime}
      contentClassName={cx(
        tab === "sessions" && "cx-app-content--sessions",
        (
          (tab === "provider" && providerMode === "list")
          || tab === "skillsMcp"
        ) && "cx-app-content--fixed",
        skillsMcpImportOpen && Boolean(skillsMcpImportPreview) && "cx-app-content--modal-locked",
      )}
    >
      <AppToast
        lang={lang}
        message={toast}
        error={error}
        loading={Boolean(providerTestingId || providerModelsLoading) && Boolean(toast)}
        onDismissMessage={() => setToast("")}
        onDismissError={() => setError("")}
      />
      {configHealth.noticeVisible && configHealth.notice && <ConfigHealthToast
        lang={lang}
        report={configHealth.notice}
        repairing={configHealth.repairing}
        onRepair={() => void configHealth.repair()}
        onDismiss={() => configHealth.dismiss(true)}
        onOpenSettings={() => {
          configHealth.dismiss();
          setSettingsGeneralRequest((value) => value + 1);
          changeTab("settings");
          openConfigurationChecks();
        }}
      />}
      <UpdateDialog
        open={updatePromptOpen && Boolean(releaseInfo.hasUpdate)}
        lang={lang}
        state={releaseInfo.updateMethod === "native" ? updater.state : undefined}
        currentVersion={aboutInfo?.appVersion}
        latestVersion={releaseInfo.latestVersion}
        onClose={() => setUpdatePromptOpen(false)}
        onUpdate={releaseInfo.updateMethod === "native" ? updater.downloadAndInstall : undefined}
        onRetry={releaseInfo.updateMethod === "native" ? updater.retry : undefined}
        onRestart={releaseInfo.updateMethod === "native" ? updater.restart : undefined}
        onDownload={() => {
          setUpdatePromptOpen(false);
          openExternalUrl(releaseInfo.htmlUrl);
        }}
      />
      <StartupWizardDialog
        open={startupWizardOpen}
        mode={startupCheckMode}
        closing={startupClosing}
        lang={lang}
        diagnostics={startupDiagnostics}
        diagnosticsError={startupDiagnosticsError ? "Environment information is unavailable. Try again; configuration checks remain available." : ""}
        configDir={configDirDraft}
        loading={loading || refreshing || startupDiagnosticsLoading || configHealth.checking || configHealth.repairing}
        configHealthPanel={normalizedConfigDirForComparison(configDirDraft.trim()) !== normalizedConfigDirForComparison(healthConfigDir)
          ? <p className="cx-config-health-note" role="status">{"The directory has changed. Choose Recheck to review its configuration."}</p>
          : <ConfigHealthPanel
          lang={lang}
          report={configHealth.report}
          checking={configHealth.checking || loading || refreshing || Boolean(actionBusy)}
          repairing={configHealth.repairing}
          error={configHealth.error}
          onCheck={() => void configHealth.check()}
          onRepair={() => void configHealth.repair()}
          onOpenConfig={() => void configHealth.openConfig()}
        />}
        onConfigDirChange={setConfigDirDraft}
        onRecheck={() => refresh(true)}
        onSkip={closeStartupWizard}
        onOpenSettings={() => {
          changeTab("settings");
          closeStartupWizard();
        }}
        onEnter={closeStartupWizard}
      />

      <PageTransition pageKey={tab}>
            {!state && tab !== "dashboard" && tab !== "settings" && tab !== "about" && (
              <CodexStateLoading lang={lang} loading={refreshing} />
            )}

            {tab === "dashboard" && (
              <OverviewPage
                lang={lang}
                ready={Boolean(state)}
                model={state?.model}
                configDir={configDirDraft}
                resolvedCodexDir={state?.codexDir || ""}
                configExists={Boolean(state?.configExists)}
                providerLabel={currentOfficialProfile?.providerName || currentProvider?.name || state?.modelProvider}
                instructionEnabled={Boolean(state?.instructionEnabled)}
                authExists={currentOfficialProfile?.isCurrent ? currentOfficialProfile.hasOwnedAuth : Boolean(state?.authExists)}
                officialAuthAvailable={currentOfficialProfile?.isCurrent ? currentOfficialProfile.hasAuth : Boolean(state?.officialAuthAvailable)}
                configPath={state?.configPath}
                modelProvider={state?.modelProvider}
                instructionPath={state
                  ? (state.instructionInjectionMode === "append"
                    ? `${state.agentsPath} (${"append"})`
                    : state.instructionFile)
                  : null}
                loading={loading || refreshing}
                hasUpdate={Boolean(releaseInfo.status === "ok" && releaseInfo.hasUpdate)}
                latestVersion={releaseInfo.latestVersion}
                onConfigDirChange={setConfigDirDraft}
                onRefresh={() => refresh(false)}
                onOpenUpdate={() => setUpdatePromptOpen(true)}
              />
            )}

            {state && tab === "provider" && (
              <ProvidersPage
                lang={lang}
                copy={getProviderPageCopy(lang)}
                mode={providerMode}
                creatingProvider={creatingProvider}
                selectedPresetId={selectedPresetId}
                selectedPresetVariantId={selectedPresetVariantId}
                onPresetSelect={(id) => applyProviderPreset(id, "")}
                onPresetVariantSelect={(id) => applyProviderPreset(selectedPresetId, id)}
                officialProfileIsDefault={editingOfficialProfileId === DEFAULT_OFFICIAL_PROFILE_ID}
                canLoadCurrentOfficial={Boolean(state.isOfficialProvider)}
                providerRows={providerPageRows}
                configDir={configDir}
                loading={loading}
                testingId={providerTestingId}
                actionBusy={actionBusy}
                editingProviderId={editingProviderId || (editingDetectedProvider ? providerForm.id : null)}
                providerForm={{
                  apiKey: providerForm.apiKey || "",
                  baseUrl: providerForm.baseUrl,
                  providerName: providerForm.providerName,
                  model: providerForm.model,
                  wireApi: providerForm.wireApi,
                  requiresOpenaiAuth: providerForm.requiresOpenaiAuth,
                }}
                officialForm={officialForm}
                officialAuthRef={officialAuthEditorRef}
                officialTomlRef={officialTomlEditorRef}
                officialInfo={{
                  officialUrl: "https://chatgpt.com/codex",
                  authPath: state.authPath,
                  current: state.isOfficialProvider ? currentOfficialProfile?.providerName || "OpenAI Official" : state.modelProvider,
                }}
                providerAuthPreview={<JsonPreview text={providerAuthPreview} />}
                providerModelMappings={providerForm.modelMappings || []}
                onProviderModelMappingsChange={(modelMappings) => setProviderForm((current) => ({ ...current, modelMappings }))}
                providerTomlDraft={providerTomlDraft}
                providerTomlRef={providerTomlEditorRef}
                apiKeyVisible={providerApiKeyVisible}
                availableModels={availableProviderModels.map((model) => model.id)}
                fetchingModels={providerModelsLoading}
                onImportCcSwitch={importFromCcSwitch}
                onAddProvider={openAddProvider}
                onLoadCcSwitchOfficial={() => void loadCcSwitchOfficial()}
                onEnableProvider={(row) => {
                  if (row.source === "official") {
                    switchOfficialProvider(row.id);
                    return;
                  }
                  const local = findLocalProviderForRow(row);
                  switchProvider(local || {
                    id: customProviderId(row.providerName),
                    providerName: row.providerName,
                    baseUrl: row.baseUrl,
                    model: row.model,
                    apiKey: row.apiKey || "",
                    tomlConfig: "",
                    wireApi: row.wireApi,
                    requiresOpenaiAuth: row.requiresOpenaiAuth,
                  });
                }}
                onTestProvider={(row) => {
                  const local = findLocalProviderForRow(row);
                  void testProvider(row.testingKey || `${row.source}-${row.id}`, row.baseUrl, local?.apiKey || row.apiKey || null);
                }}
                onEditProvider={(row) => {
                  if (row.source === "official") {
                    void openOfficialEdit(row.id);
                    return;
                  }
                  const local = findLocalProviderForRow(row);
                  if (local) openEditProvider(local);
                  else if (row.source === "detected") openEditDetectedProvider(row);
                }}
                onDuplicateProvider={(row) => {
                  if (row.source === "official") {
                    void duplicateOfficialProfile(row);
                    return;
                  }
                  void duplicateProvider(row);
                }}
                onDeleteProvider={(row) => {
                  if (row.source === "official") return removeOfficialProfile(row.id, row.isCurrent);
                  const local = findLocalProviderForRow(row);
                  return local ? removeProvider(local.id, row.isCurrent) : Promise.resolve(false);
                }}
                onResetOfficial={resetOfficialProvider}
                onCancelMode={() => {
                  officialDraftRequestRef.current += 1;
                  setProviderMode("list");
                  setCreatingProvider(false);
                  setEditingDetectedProvider(false);
                  setProviderTomlDirty(false);
                  setProviderCommonConfigDirty(false);
                }}
                onOfficialModelChange={(value) => setOfficialForm((current) => ({ ...current, model: value }))}
                onOfficialNameChange={(value) => setOfficialForm((current) => ({ ...current, providerName: value }))}
                onLoadCurrentOfficial={() => void loadCurrentOfficial()}
                onOfficialAuthChange={(value) => {
                  setOfficialAuthDirty(true);
                  setOfficialForm((current) => ({ ...current, authJson: value }));
                }}
                onOfficialConfigChange={(value) => setOfficialForm((current) => ({ ...current, configText: value }))}
                onSaveOfficial={saveOfficialConfig}
                onApiKeyChange={(value) => {
                  resetAvailableProviderModels();
                  setProviderForm((current) => ({ ...current, apiKey: value }));
                }}
                onBaseUrlChange={(value) => {
                  resetAvailableProviderModels();
                  setProviderForm((current) => ({ ...current, baseUrl: value }));
                }}
                onProviderNameChange={(value) => setProviderForm((current) => ({
                  ...current,
                  providerName: value,
                  id: editingProviderId || uniqueId(customProviderId(value), savedProviders.map((provider) => provider.id)),
                }))}
                onProviderModelChange={(value) => setProviderForm((current) => ({ ...current, model: value }))}
                onFetchModels={() => void fetchProviderModels()}
                onWireApiChange={(value) => setProviderForm((current) => ({ ...current, wireApi: value }))}
                onRequiresAuthChange={(value) => setProviderForm((current) => ({ ...current, requiresOpenaiAuth: value }))}
                onToggleApiKeyVisibility={() => setProviderApiKeyVisible((value) => !value)}
                onProviderTomlDraftChange={(value, origin = "manual") => {
                  providerDraftRequestRef.current += 1;
                  setProviderTomlDraft(value);
                  setProviderTomlDirty(true);
                  if (origin === "manual") setProviderCommonConfigDirty(true);
                }}
                onResetProviderToml={() => {
                  providerDraftRequestRef.current += 1;
                  setProviderTomlDraft(
                    providerForm.tomlConfig?.trim()
                    || state?.configText?.trim()
                    || providerTomlPreview,
                  );
                  setProviderTomlDirty(false);
                  setProviderCommonConfigDirty(false);
                  setProviderDraftRefreshToken((token) => token + 1);
                }}
                onSaveProvider={saveProviderConfig}
              />
            )}

            {state && (tab === "sessions" || visitedTabs.has("sessions")) && (
              <SessionManagementPage
                active={tab === "sessions"}
                lang={lang}
                sessionStatus={sessionStatus}
                sessionHasMismatches={sessionHasMismatches}
                sessionSyncCount={sessionSyncCount}
                sessionTargetLabel={sessionTargetLabel}
                sessionVisibleTotal={sessionVisibleTotal}
                sessionPreviewTruncated={sessionPreviewTruncated}
                visibleSessions={visibleSessions}
                filteredSessions={filteredSessions}
                allSessionsByCwd={allSessionsByCwd}
                groupedSessions={groupedSessions}
                selectedSessionIds={selectedSessionIds}
                selectedSessionSet={selectedSessionSet}
                selectedSessions={selectedSessions}
                sessionQuery={sessionQuery}
                sessionGroupByCwd={sessionGroupByCwd}
                showInternalSessions={showInternalSessions}
                loading={loading}
                actionBusy={actionBusy}
                sessionDeleteConfirmOpen={sessionDeleteConfirmOpen}
                sessionDeleteBusy={sessionDeleteBusy}
                sessionDeleteSafetyConfirmed={sessionDeleteSafetyConfirmed}
                sessionExportBusy={sessionExportBusy}
                onExportSessions={exportSessions}
                onCheckSessions={checkSessions}
                onSyncSessions={syncSessions}
                onSessionQueryChange={(value) => {
                  setSessionQuery(value);
                  setSelectedSessionIds([]);
                  setSessionDeleteConfirmOpen(false);
                }}
                onSessionGroupByCwdChange={setSessionGroupByCwd}
                onShowInternalSessionsChange={(checked) => {
                  setShowInternalSessions(checked);
                  setSelectedSessionIds([]);
                  setSessionDeleteConfirmOpen(false);
                }}
                onOpenDeleteConfirm={() => {
                  setSessionDeleteSafetyConfirmed(false);
                  setSessionDeleteConfirmOpen(true);
                }}
                onToggleSessionSelected={toggleSessionSelected}
                onSetSessionGroupSelected={setSessionGroupSelected}
                onCloseDeleteConfirm={closeSessionDeleteConfirm}
                onDeleteSelectedSessions={deleteSelectedSessions}
                onDeleteSafetyConfirmedChange={setSessionDeleteSafetyConfirmed}
              />
            )}

            {state && (tab === "skillsMcp" || visitedTabs.has("skillsMcp")) && (
              <SkillsMcpPage
                lang={lang}
                state={skillsMcpState}
                activeTab={skillsMcpTab}
                actionBusy={actionBusy}
                importOpen={skillsMcpImportOpen}
                importPreview={skillsMcpImportPreview}
                className={tab !== "skillsMcp" ? "page-pane-hidden" : undefined}
                onTabChange={setSkillsMcpTab}
                onLoad={loadSkillsMcp}
                onOpenImportPreview={openImportExistingSkillsMcpPreview}
                onCloseImportPreview={() => setSkillsMcpImportOpen(false)}
                onConfirmImport={importExistingSkillsMcp}
                onInstallZip={installSkillZipFile}
                onExport={exportSkillsMcp}
                onCheckUpdates={checkSkillUpdatesAction}
                onToggleSkill={toggleSkillEnabled}
                onToggleMcp={toggleMcpEnabled}
                noteBusyKey={skillsMcpNoteBusy}
                onSaveNote={saveSkillsMcpNote}
              />
            )}

            {state && tab === "instruction" && (
              <PromptsPage
                lang={lang}
                instructionMode={instructionMode}
                promptForm={promptForm}
                editingPromptId={editingPromptId}
                editingBuiltinPrompt={editingBuiltinPrompt}
                loading={loading || promptDetailLoading}
                actionBusy={actionBusy}
                promptSyncing={promptSyncing}
                promptCatalogReady={promptCatalogReady}
                promptImportRef={promptImportRef}
                promptInjectionMode={promptInjectionMode}
                promptModeHelpOpen={promptModeHelpOpen}
                promptModeHelpRef={promptModeHelpRef}
                instructionEnabled={state.instructionEnabled}
                activeInstructionTitle={activeInstructionTitle}
                activeInjectionMode={state.instructionInjectionMode}
                instructionTemplates={instructionTemplates}
                builtinPromptStatuses={builtinPromptStatus}
                activeBuiltinTemplateId={activeBuiltinTemplateId}
                orphanedBuiltinPrompt={missingActiveBuiltinTemplateId ? {
                  id: missingActiveBuiltinTemplateId,
                  title: activeInstructionTitle,
                  description: "This template was removed online but is still active.",
                } : null}
                savedPrompts={savedPrompts}
                managedSavedPromptId={state.instructionTemplateKey?.startsWith("saved:")
                  ? state.instructionTemplateKey.slice("saved:".length)
                  : null}
                preservedSavedPromptFilename={state.instructionInjectionMode === "append" ? currentInstructionFilename : null}
                externalPrompt={state.instructionFile
                  && currentInstructionId === "custom"
                  && !savedPrompts.some((prompt) => currentInstructionFilename === prompt.filename)
                  && !(missingActiveBuiltinTemplateId && state.instructionInjectionMode !== "append")
                  ? {
                    title: "Existing user prompt",
                    description: state.instructionInjectionMode === "append"
                      ? "Append mode preserves this external prompt alongside the AstraX AGENTS.md block."
                      : "This external prompt is not managed by AstraX.",
                    filename: currentInstructionFilename,
                  }
                  : null}
                onSyncBuiltinPrompts={() => refreshBuiltinPrompts()}
                onImportPrompt={importPromptMd}
                onAddPrompt={openAddPrompt}
                onInstructionModeChange={(mode) => {
                  invalidatePromptDetail();
                  setInstructionMode(mode);
                }}
                onPromptInjectionModeChange={setPromptInjectionMode}
                onTogglePromptModeHelp={() => setPromptModeHelpOpen((open) => !open)}
                onEnableBuiltinPrompt={switchInstructionTemplate}
                onDisableInstruction={disableInstruction}
                onEnableSavedPrompt={enableSavedPrompt}
                onDisableExternalPrompt={disableExternalInstruction}
                onEditPrompt={openEditPrompt}
                onEditBuiltinPrompt={openEditBuiltinPrompt}
                onDeletePrompt={removeSavedPrompt}
                onPromptFormFieldChange={(field, value) => setPromptForm((current) => ({
                  ...current,
                  [field]: value,
                  ...(field === "title" ? { id: editingPromptId || providerId(value) } : {}),
                }))}
                onSavePrompt={savePromptOnly}
              />
            )}

            {state && tab === "toml" && (
              <TomlConfigPage
                eyebrow="~/.codex/config.toml"
                title={t.toml.title}
                description={t.toml.desc}
                loaded={state.configExists ? t.toml.loaded : t.dashboard.missing}
                isLoaded={state.configExists}
                preview={<TomlPreview text={state.configText || t.toml.missingText} />}
              />
            )}

            {tab === "about" && (
              <AboutPage
                copy={{
                  eyebrow: "About",
                  title: "About AstraX",
                  appVersionLabel: `AstraX ${"Version"}`,
                  codexVersionLabel: `Codex CLI ${"Version"}`,
                  codexHomeLabel: "CODEX_HOME",
                  projectLabel: "Project",
                  openProjectLabel: "Open project",
                  openIssuesLabel: "Issues",
                  releasesEyebrow: "GitHub Releases",
                  releasesTitle: "Update check",
                  releaseStatusLabel: "Status",
                  latestVersionLabel: "Latest version",
                  checkUpdateLabel: "Check updates",
                  openReleasesLabel: "Open releases",
                }}
                appVersion={aboutInfo?.appVersion || (aboutLoading ? "Detecting" : "-")}
                codexVersion={aboutInfo?.codexVersion || (aboutLoading
                  ? "Detecting..."
                  : "Not detected")}
                codexHome={aboutInfo?.codexDir || state?.codexDir || configDir || "~/.codex"}
                projectUrl={aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`}
                release={{
                  status: releaseStatusLabel,
                  latestVersion: releaseInfo.latestVersion || "-",
                  tone: releaseInfo.status === "error"
                    ? "error"
                    : releaseInfo.hasUpdate
                      ? "warning"
                      : releaseInfo.status === "ok"
                        ? "success"
                        : "neutral",
                  checking: releaseInfo.status === "checking"
                    || updater.state.phase === "downloading"
                    || updater.state.phase === "installing",
                  canOpenReleases: Boolean(releaseInfo.htmlUrl),
                }}
                onOpenProject={() => openExternalUrl(aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`)}
                onOpenIssues={() => openExternalUrl(`${aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`}/issues`)}
                onCheckUpdate={() => void checkForUpdates()}
                onOpenReleases={() => openExternalUrl(releaseInfo.htmlUrl)}
              />
            )}

            {tab === "settings" && (
              <SettingsPage
                lang={lang}
                configDir={configDir}
                onChange={async () => { routingRequestRefreshRef.current(); }}
                generalRequest={settingsGeneralRequest}
                configHealthStatus={<ConfigHealthStatus
                  lang={lang}
                  report={configHealth.report}
                  checking={configHealth.checking || loading || refreshing || Boolean(actionBusy)}
                  repairing={configHealth.repairing}
                  error={configHealth.error}
                />}
                copy={{
                  eyebrow: "Settings",
                  title: t.settings.title,
                  productTitle: t.settings.productName,
                  productDescription: t.settings.productDesc,
                  productValue: "AstraX",
                  recheckTitle: "Environment & configuration check",
                  recheckDescription: "Review your Codex environment and configuration, and repair issues when needed.",
                  recheckLabel: "Check",
                  restartTitle: "Codex desktop app",
                  restartDescription: "Restart the local Codex (ChatGPT) desktop app without restarting AstraX.",
                  restartLabel: "Restart Codex",
                  restartTargetLabel: "Codex (ChatGPT) desktop app",
                  restartConfirmTitle: "Restart Codex?",
                  restartConfirmDescription: "The running Codex window will close and reopen. Unsaved input may be lost.",
                  restartCancelLabel: "Cancel",
                  restartConfirmLabel: "Restart",
                  restartingLabel: "Restarting",
                }}
                recheckBusy={loading || refreshing}
                restartBusy={restartCodexBusy}
                onRestartCodex={restartCodexDesktop}
                onRecheck={openConfigurationChecks}
              />
            )}
      </PageTransition>
    </AppShell>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode><App /></React.StrictMode>,
);
