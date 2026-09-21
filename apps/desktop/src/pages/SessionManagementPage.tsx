import {
  AlertCircle,
  CheckCircle2,
  Download,
  FolderTree,
  History,
  Info,
  Loader2,
  RefreshCw,
  Search,
  Trash2,
  Zap,
} from "lucide-react";
import { Button, Checkbox, ModalShell, cx } from "../components/ui";
import "../styles/session-management.css";

export type Lang = "zh" | "en";

export type SessionPreview = {
  id: string;
  title: string;
  modelProvider?: string | null;
  model?: string | null;
  cwd?: string | null;
  rolloutPath?: string | null;
  updatedAtMs?: number | null;
  archived: boolean;
  hasUserEvent: boolean;
  isSubagent: boolean;
  needsSync: boolean;
};

export type SessionSyncStatus = {
  codexDir: string;
  targetProvider: string;
  rolloutFiles: number;
  sessionMetaCount: number;
  mismatchedRollouts: number;
  mismatchedSessionMeta: number;
  sqliteDbs: number;
  sqliteThreads: number;
  topLevelThreads: number;
  subagentThreads: number;
  mismatchedThreads: number;
  mismatchedSessions: number;
  needsSync: boolean;
  scanComplete: boolean;
  scanFailures: string[];
  backupDir?: string | null;
  warnings: string[];
  sessions: SessionPreview[];
};

type SessionManagementPageProps = {
  active: boolean;
  lang: Lang;
  sessionStatus: SessionSyncStatus | null;
  sessionHasMismatches: boolean;
  sessionSyncCount: number;
  sessionTargetLabel: string;
  sessionVisibleTotal: number;
  sessionPreviewTruncated: boolean;
  visibleSessions: SessionPreview[];
  filteredSessions: SessionPreview[];
  allSessionsByCwd: Map<string, SessionPreview[]>;
  groupedSessions: Array<[string, SessionPreview[]]>;
  selectedSessionIds: string[];
  selectedSessionSet: Set<string>;
  selectedSessions: SessionPreview[];
  sessionQuery: string;
  sessionGroupByCwd: boolean;
  showInternalSessions: boolean;
  loading: boolean;
  actionBusy: string;
  sessionDeleteConfirmOpen: boolean;
  sessionDeleteBusy: boolean;
  sessionExportBusy: boolean;
  sessionDeleteSafetyConfirmed: boolean;
  onCheckSessions: () => void;
  onSyncSessions: () => void;
  onSessionQueryChange: (value: string) => void;
  onSessionGroupByCwdChange: (checked: boolean) => void;
  onShowInternalSessionsChange: (checked: boolean) => void;
  onOpenDeleteConfirm: () => void;
  onToggleSessionSelected: (id: string) => void;
  onSetSessionGroupSelected: (sessions: SessionPreview[], checked: boolean) => void;
  onCloseDeleteConfirm: () => void;
  onDeleteSelectedSessions: () => void;
  onExportSessions: (ids: string[]) => void;
  onDeleteSafetyConfirmedChange: (checked: boolean) => void;
};

function formatSessionTime(value?: number | null, lang: Lang = "zh") {
  if (!value) return "Unknown time";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "Unknown time";
  return date.toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function compactPath(value: string | null | undefined, max = 58, missing = "No path recorded") {
  if (!value) return missing;
  const normalized = value.replace(/\\/g, "/");
  if (normalized.length <= max) return normalized;
  const parts = normalized.split("/").filter(Boolean);
  if (parts.length >= 3) {
    const tail = parts.slice(-3).join("/");
    return `…/${tail}`;
  }
  return `…${normalized.slice(-max + 1)}`;
}

function shortId(value: string) {
  return value.length > 13 ? `${value.slice(0, 8)}…${value.slice(-4)}` : value;
}

export function SessionManagementPage({
  active,
  lang,
  sessionStatus,
  sessionHasMismatches,
  sessionSyncCount,
  sessionTargetLabel,
  sessionVisibleTotal,
  sessionPreviewTruncated,
  visibleSessions,
  filteredSessions,
  allSessionsByCwd,
  groupedSessions,
  selectedSessionIds,
  selectedSessionSet,
  selectedSessions,
  sessionQuery,
  sessionGroupByCwd,
  showInternalSessions,
  loading,
  actionBusy,
  sessionDeleteConfirmOpen,
  sessionDeleteBusy,
  sessionExportBusy,
  sessionDeleteSafetyConfirmed,
  onCheckSessions,
  onSyncSessions,
  onSessionQueryChange,
  onSessionGroupByCwdChange,
  onShowInternalSessionsChange,
  onOpenDeleteConfirm,
  onToggleSessionSelected,
  onSetSessionGroupSelected,
  onCloseDeleteConfirm,
  onDeleteSelectedSessions,
  onExportSessions,
  onDeleteSafetyConfirmedChange,
}: SessionManagementPageProps) {
  const copy = {
        syncEyebrow: "SESSION SYNC",
        title: "Session management",
        description: "Keep user conversations in one shared history. Internal tasks are excluded from sync; chat content is unchanged.",
        syncTo: "Sync to",
        check: "Check sessions",
        checking: "Checking...",
        sync: "Sync sessions",
        syncing: "Syncing...",
        clickToCheck: "Check sessions to get started",
        scanIncomplete: "Unable to verify sync status. See the reason below.",
        needsSync: (count: number) => `${count} session(s) need syncing`,
        allSynced: "User conversations are synced",
        sessionCount: (count: number) => `${count} sessions`,
        local: "LOCAL SESSIONS",
        list: "Sessions",
        shown: (shown: number, total: number) => `${shown} / ${total} shown`,
        loaded: (count: number) => `${count} loaded`,
        search: "Search title / project / provider / ID",
        groupByProject: "Group by project path",
        showInternal: (count: number) => `Show internal sessions (${count})`,
        deleteSelected: "Delete selected",
        exportSelected: "Export selected",
        exportOne: "Export as Markdown",
        exporting: "Exporting…",
        exportHint: "Save one session as Markdown, or multiple sessions in a ZIP",
        deleteMany: (count: number) => `Delete ${count} permanently`,
        selectAll: "Select all sessions in the current list",
        selectProject: (path: string, count: number) => `Select ${count} sessions in ${path}`,
        projectCount: (count: number, truncated: boolean) => `${count}${truncated ? " loaded" : ""}`,
        projectShown: (shown: number, total: number) => `${shown} / ${total} shown`,
        selectSession: "Select session",
        archived: "Archived",
        internal: "Internal",
        internalHint: "Internal tasks are available for inspection and excluded from session sync.",
        pending: "Needs sync",
        unknownProvider: "Unknown provider",
        noModel: "Not recorded",
        noMatch: "No matching sessions.",
        noSessions: "No sessions loaded. Click Check sessions to refresh.",
        diagnostics: "Diagnostics",
        diagnosticsCount: (count: number) => `${count} · click to view`,
        deleteTitle: (count: number) => `Permanently delete ${count} session(s)`,
        irreversible: "This cannot be undone",
        deleteDescription: "Selected sessions will be permanently deleted from Codex local data. There is no recycle bin or new backup.",
        deleteChildren: "Child sessions spawned from these sessions will also be deleted.",
        closeClients: "Close other Codex windows or CLIs using these sessions first.",
        pendingDelete: "Sessions to delete",
        moreSessions: (count: number) => `${count} more session(s) not shown`,
        safetyCheck: "I closed other Codex windows or CLIs using these sessions",
        cancel: "Cancel",
        deleting: "Deleting permanently...",
        confirmDelete: (count: number) => `Delete ${count} permanently`,
      };

  const scanIncomplete = Boolean(sessionStatus && !sessionStatus.scanComplete);
  const diagnostics = [
    ...(sessionStatus?.scanFailures || []).map((message) => ({ message, blocking: true })),
    ...(sessionStatus?.warnings || []).map((message) => ({ message, blocking: false })),
  ];
  const dialogOpen = sessionDeleteConfirmOpen && selectedSessions.length > 0;
  const selectedVisibleCount = filteredSessions.filter((item) => selectedSessionSet.has(item.id)).length;
  const allVisibleSelected = filteredSessions.length > 0 && selectedVisibleCount === filteredSessions.length;
  const visibleSelectionIsPartial = selectedVisibleCount > 0 && !allVisibleSelected;

  return (
    <>
      <ModalShell
        open={dialogOpen}
        onClose={onCloseDeleteConfirm}
        title={copy.deleteTitle(selectedSessions.length)}
        description={copy.deleteDescription}
        size="lg"
        closeLabel={"Close"}
        closeOnBackdrop={!sessionDeleteBusy}
        closeOnEscape={!sessionDeleteBusy}
        showCloseButton={!sessionDeleteBusy}
        className="cx-session-delete-dialog"
        bodyClassName="cx-session-delete-modal-body"
        footer={(
          <>
            <Button variant="secondary" onClick={onCloseDeleteConfirm} disabled={sessionDeleteBusy} data-initial-focus>
              {copy.cancel}
            </Button>
            <Button
              variant="danger"
              className="cx-session-delete-confirm"
              icon={sessionDeleteBusy ? <Loader2 size={16} className="cx-session-spin" aria-hidden="true" /> : <Trash2 size={16} aria-hidden="true" />}
              onClick={onDeleteSelectedSessions}
              disabled={sessionDeleteBusy || !sessionDeleteSafetyConfirmed}
            >
              {sessionDeleteBusy ? copy.deleting : copy.confirmDelete(selectedSessions.length)}
            </Button>
          </>
        )}
      >
        <div className="cx-session-delete-warning">
          <AlertCircle size={19} strokeWidth={1.9} aria-hidden="true" />
          <div>
            <strong>{copy.irreversible}</strong>
            <p>{copy.deleteChildren}</p>
            <p>{copy.closeClients}</p>
          </div>
        </div>

        <div className="cx-session-delete-list" aria-label={copy.pendingDelete}>
          {selectedSessions.slice(0, 8).map((item) => (
            <div className="cx-session-delete-item" key={item.id}>
              <strong title={item.title}>{item.title || "Untitled session"}</strong>
              <code>#{shortId(item.id)}</code>
              <span title={item.cwd || item.rolloutPath || undefined}>
                {compactPath(item.cwd || item.rolloutPath, 72, "No path recorded")}
              </span>
            </div>
          ))}
          {selectedSessions.length > 8 && <p className="cx-session-delete-more">{copy.moreSessions(selectedSessions.length - 8)}</p>}
        </div>

        <Checkbox
          className="cx-session-safety-check"
          checked={sessionDeleteSafetyConfirmed}
          onCheckedChange={onDeleteSafetyConfirmedChange}
          disabled={sessionDeleteBusy}
          label={copy.safetyCheck}
        />
      </ModalShell>

      <section className={cx("cx-session-page", !active && "page-pane-hidden")}>
        <header className="cx-session-header">
          <div className="cx-session-heading">
            <p className="cx-session-eyebrow"><RefreshCw size={13} strokeWidth={2} aria-hidden="true" />{copy.syncEyebrow}</p>
            <h2>{copy.title}</h2>
            <p className="cx-session-description">{copy.description}</p>
          </div>
          <div className="cx-session-header-actions">
            <span className="cx-session-target"><span>{copy.syncTo}</span><strong>{sessionTargetLabel}</strong></span>
            <button type="button" className="cx-session-button cx-session-button--secondary" onClick={onCheckSessions} disabled={loading} aria-busy={actionBusy === "checkSessions"}>
              {actionBusy === "checkSessions" ? <Loader2 size={16} className="cx-session-spin" aria-hidden="true" /> : <RefreshCw size={16} aria-hidden="true" />}
              {actionBusy === "checkSessions" ? copy.checking : copy.check}
            </button>
            <button type="button" className="cx-session-button cx-session-button--primary" onClick={onSyncSessions} disabled={loading || scanIncomplete || !sessionHasMismatches} aria-busy={actionBusy === "syncSessions"}>
              {actionBusy === "syncSessions" ? <Loader2 size={16} className="cx-session-spin" aria-hidden="true" /> : <Zap size={16} aria-hidden="true" />}
              {actionBusy === "syncSessions" ? copy.syncing : copy.sync}
            </button>
          </div>
        </header>

        <div className={cx("cx-session-summary", scanIncomplete || sessionHasMismatches ? "cx-session-summary--needs-sync" : "cx-session-summary--synced")}>
          <span className="cx-session-summary-status">
            {!sessionStatus ? <Info size={15} aria-hidden="true" /> : scanIncomplete || sessionHasMismatches ? <AlertCircle size={15} aria-hidden="true" /> : <CheckCircle2 size={15} aria-hidden="true" />}
            {!sessionStatus ? copy.clickToCheck : scanIncomplete ? copy.scanIncomplete : sessionHasMismatches ? copy.needsSync(sessionSyncCount) : copy.allSynced}
          </span>
          <span className="cx-session-summary-count">{copy.sessionCount(sessionStatus?.topLevelThreads ?? 0)}</span>
        </div>

        <div className="cx-session-list-card">
          <div className="cx-session-list-heading">
            <div>
              <p className="cx-session-section-label">{copy.local}</p>
              <h3>{copy.list}</h3>
            </div>
            <span
              className="cx-session-total"
              title={sessionPreviewTruncated ? copy.loaded(visibleSessions.length) : undefined}
            >
              {copy.shown(filteredSessions.length, sessionVisibleTotal)}
            </span>
          </div>

          <div className="cx-session-toolbar">
            <label className="cx-session-search">
              <Search size={16} strokeWidth={1.9} aria-hidden="true" />
              <input
                value={sessionQuery}
                onChange={(event) => onSessionQueryChange(event.target.value)}
                placeholder={copy.search}
                aria-label={copy.search}
              />
            </label>
            <Checkbox
              className={cx("cx-session-toggle", sessionGroupByCwd && "cx-session-toggle--active")}
              checked={sessionGroupByCwd}
              onCheckedChange={onSessionGroupByCwdChange}
              label={<><FolderTree size={15} strokeWidth={1.9} aria-hidden="true" /><span>{copy.groupByProject}</span></>}
            />
            {(sessionStatus?.subagentThreads ?? 0) > 0 && (
              <Checkbox
                className={cx("cx-session-toggle", showInternalSessions && "cx-session-toggle--active")}
                checked={showInternalSessions}
                onCheckedChange={onShowInternalSessionsChange}
                title={copy.internalHint}
                label={copy.showInternal(sessionStatus?.subagentThreads ?? 0)}
              />
            )}
            <button
              type="button"
              className="cx-session-button cx-session-button--secondary"
              onClick={() => onExportSessions(selectedSessionIds)}
              disabled={loading || sessionDeleteBusy || sessionExportBusy || selectedSessionIds.length === 0}
              title={copy.exportHint}
              aria-busy={sessionExportBusy}
            >
              {sessionExportBusy ? <Loader2 size={15} className="cx-session-spin" aria-hidden="true" /> : <Download size={15} strokeWidth={1.9} aria-hidden="true" />}
              {sessionExportBusy ? copy.exporting : copy.exportSelected}
            </button>
            <button
              type="button"
              className={cx("cx-session-button cx-session-delete-trigger", selectedSessionIds.length > 0 ? "cx-session-button--danger" : "cx-session-button--secondary")}
              onClick={onOpenDeleteConfirm}
              disabled={loading || sessionDeleteBusy || sessionExportBusy || selectedSessionIds.length === 0}
              title={selectedSessionIds.length > 0 ? undefined : copy.deleteSelected}
            >
              <Trash2 size={15} strokeWidth={1.9} aria-hidden="true" />
              {selectedSessionIds.length > 0 ? copy.deleteMany(selectedSessionIds.length) : copy.deleteSelected}
            </button>
          </div>

          {filteredSessions.length > 0 ? (
            <div className="cx-session-scroll" role="table" aria-label={copy.list}>
              <div className="cx-session-column-head" role="row">
                <Checkbox
                  className="cx-session-select-all"
                  checked={allVisibleSelected}
                  indeterminate={visibleSelectionIsPartial}
                  onCheckedChange={(checked) => onSetSessionGroupSelected(filteredSessions, checked)}
                  aria-label={copy.selectAll}
                  disabled={loading || sessionDeleteBusy}
                />
                <span>{"Session"}</span>
                <span>{"Updated"}</span>
                <span>{"Provider"}</span>
                <span>{"Model"}</span>
                <span className="cx-session-id-heading">ID</span>
                <span className="cx-session-actions-heading">{"Export"}</span>
              </div>
              <div className="cx-session-table-body">
                {groupedSessions.map(([group, items]) => {
                  const projectSessions = allSessionsByCwd.get(group) || items;
                  const selectedProjectCount = projectSessions.filter((item) => selectedSessionSet.has(item.id)).length;
                  const projectSelected = projectSessions.length > 0 && selectedProjectCount === projectSessions.length;
                  const projectPartiallySelected = selectedProjectCount > 0 && !projectSelected;
                  const groupCountLabel = items.length === projectSessions.length
                    ? copy.projectCount(projectSessions.length, sessionPreviewTruncated)
                    : copy.projectShown(items.length, projectSessions.length);
                  return (
                    <div className="cx-session-group" key={group}>
                      {sessionGroupByCwd && (
                        <label className={cx("cx-session-group-heading", projectSelected && "cx-session-group-heading--selected", projectPartiallySelected && "cx-session-group-heading--partial")}>
                          <input
                            className="cx-session-checkbox"
                            type="checkbox"
                            ref={(input) => {
                              if (input) input.indeterminate = projectPartiallySelected;
                            }}
                            checked={projectSelected}
                            onChange={(event) => onSetSessionGroupSelected(projectSessions, event.target.checked)}
                            aria-label={copy.selectProject(group, projectSessions.length)}
                          />
                          <span title={group}>{compactPath(group, 96, "No path recorded")}</span>
                          <em>{groupCountLabel}</em>
                        </label>
                      )}
                      {items.map((item) => (
                        <div
                          className={cx("cx-session-row", item.needsSync && !item.isSubagent && "cx-session-row--needs-sync", selectedSessionSet.has(item.id) && "cx-session-row--selected")}
                          key={item.id}
                          role="row"
                          onClick={(event) => {
                            if (!(event.target as HTMLElement).closest("button, input") && !loading && !sessionDeleteBusy) onToggleSessionSelected(item.id);
                          }}
                        >
                          <span className="cx-session-select-box" title={copy.selectSession}>
                            <input
                              className="cx-session-checkbox"
                              type="checkbox"
                              checked={selectedSessionSet.has(item.id)}
                              disabled={loading || sessionDeleteBusy}
                              onChange={() => onToggleSessionSelected(item.id)}
                              aria-label={`${copy.selectSession}: ${item.title || "Untitled session"} (#${shortId(item.id)})`}
                            />
                          </span>
                          <div className="cx-session-row-copy">
                            <div className="cx-session-row-title">
                              <strong title={item.title}>{item.title || "Untitled session"}</strong>
                              {item.archived && <span className="cx-session-state">{copy.archived}</span>}
                              {item.isSubagent && <span className="cx-session-state" title={copy.internalHint}>{copy.internal}</span>}
                              {item.needsSync && !item.isSubagent && <span className="cx-session-state cx-session-state--warn">{copy.pending}</span>}
                            </div>
                            {!sessionGroupByCwd && <p title={item.cwd || item.rolloutPath || undefined}>{compactPath(item.cwd || item.rolloutPath, 72, "No path recorded")}</p>}
                          </div>
                          <span className="cx-session-meta cx-session-meta--time" title={item.updatedAtMs ? new Date(item.updatedAtMs).toLocaleString() : undefined}>{formatSessionTime(item.updatedAtMs, lang)}</span>
                          <code className="cx-session-meta cx-session-meta--provider" title={item.modelProvider || undefined}>{item.modelProvider || copy.unknownProvider}</code>
                          <span className="cx-session-meta cx-session-meta--model" title={item.model || undefined}>{item.model || copy.noModel}</span>
                          <small className="cx-session-meta cx-session-meta--id" title={item.id}>#{shortId(item.id)}</small>
                          <button
                            type="button"
                            className="cx-session-row-export"
                            onClick={() => onExportSessions([item.id])}
                            disabled={loading || sessionDeleteBusy || sessionExportBusy}
                            aria-label={`${copy.exportOne}: ${item.title || "Untitled session"}`}
                            title={copy.exportOne}
                          >
                            <Download size={15} strokeWidth={1.9} aria-hidden="true" />
                          </button>
                        </div>
                      ))}
                    </div>
                  );
                })}
              </div>
            </div>
          ) : (
            <div className="cx-session-empty">
              <History size={22} strokeWidth={1.7} aria-hidden="true" />
              <span>{sessionQuery ? copy.noMatch : copy.noSessions}</span>
            </div>
          )}
        </div>

        {diagnostics.length ? (
          <details className="cx-session-diagnostics" open={scanIncomplete || undefined}>
            <summary>
              <AlertCircle size={15} strokeWidth={1.9} aria-hidden="true" />
              <span>{copy.diagnostics}</span>
              <small>{copy.diagnosticsCount(diagnostics.length)}</small>
            </summary>
            <div className="cx-session-diagnostic-items">
              {diagnostics.map((item, index) => <p key={`${index}-${item.message}`}>{item.blocking ? <AlertCircle size={14} aria-hidden="true" /> : <Info size={14} aria-hidden="true" />}{item.message}</p>)}
            </div>
          </details>
        ) : null}
      </section>
    </>
  );
}
