import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { configHealthIssueKey, createConfigHealthMonitor, createConfigHealthNoticeRegistry } from "./configHealthMonitor";
import type { ConfigHealthRepairResult, ConfigHealthReport } from "./configHealthTypes";

export function useConfigHealth(options: {
  configDir: string;
  canCheck: boolean;
  canNotify: boolean;
  reviewing?: boolean;
  lang: "zh" | "en";
  onHint: (text: string) => void;
  onRepaired: () => void;
}) {
  const runtime = useRef(options); runtime.current = options;
  const scope = useRef(options.configDir); scope.current = options.configDir;
  const mounted = useRef(true);
  const revision = useRef(0);
  const busyRef = useRef(false);
  const monitor = useRef<ReturnType<typeof createConfigHealthMonitor> | null>(null);
  const registry = useRef<ReturnType<typeof createConfigHealthNoticeRegistry> | null>(null);
  if (!registry.current) {
    let storage: Storage | undefined;
    try { storage = window.localStorage; } catch { /* In-memory fallback. */ }
    registry.current = createConfigHealthNoticeRegistry(storage);
  }
  const [report, setReport] = useState<ConfigHealthReport | null>(null);
  const [notice, setNotice] = useState<ConfigHealthReport | null>(null);
  const [checking, setChecking] = useState(false);
  const [repairing, setRepairing] = useState(false);
  const [error, setError] = useState("");
  const [visible, setVisible] = useState(document.visibilityState !== "hidden");
  const [modalOpen, setModalOpen] = useState(false);
  const modalOpenRef = useRef(modalOpen); modalOpenRef.current = modalOpen;
  const reportRef = useRef(report); reportRef.current = report;
  const noticeRef = useRef(notice); noticeRef.current = notice;
  const noticeTime = useRef<{ issue: string; remaining: number } | null>(null);

  const accept = useCallback((next: ConfigHealthReport) => {
    if (runtime.current.reviewing && next.status === "issues") registry.current?.mark(next);
    setReport((previous) => previous?.fingerprint === next.fingerprint && previous.status === next.status ? previous : next);
    const existing = noticeRef.current;
    if (next.status === "issues" && existing && configHealthIssueKey(next) === configHealthIssueKey(existing)) {
      // Refresh the repair token without shortening an existing notice merely
      // because an editor changed a comment or another unrelated setting.
      setNotice((previous) => previous?.fingerprint === next.fingerprint ? previous : next);
    }
  }, []);

  useEffect(() => {
    if (options.reviewing && report?.status === "issues") registry.current?.mark(report);
  }, [options.reviewing, report]);

  useEffect(() => {
    const update = () => setModalOpen(Boolean(document.querySelector('[aria-modal="true"]')));
    const observer = new MutationObserver(update);
    observer.observe(document.body, { childList: true, subtree: true });
    update();
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; revision.current += 1; };
  }, []);

  useEffect(() => {
    revision.current += 1;
    busyRef.current = false;
    setReport(null); setNotice(null); setChecking(false); setRepairing(false); setError("");
    const directory = options.configDir;
    const instance = createConfigHealthMonitor({
      read: () => invoke<ConfigHealthReport>("check_codex_config", { configDir: directory || null }),
      revision: () => revision.current,
      isVisible: () => document.visibilityState !== "hidden",
      canRead: () => !busyRef.current && runtime.current.canCheck && scope.current === directory,
      canNotify: () => runtime.current.canNotify && !modalOpenRef.current,
      onReport: accept,
      onRecovered: (next) => { registry.current?.clear(next); setNotice(null); },
      onProblem: (next) => {
        if (registry.current?.has(next)) return;
        registry.current?.mark(next);
        noticeTime.current = { issue: configHealthIssueKey(next), remaining: 30_000 };
        setNotice(next);
      },
    });
    monitor.current = instance;
    const wake = () => {
      setVisible(document.visibilityState !== "hidden");
      instance.wake();
    };
    window.addEventListener("focus", wake);
    document.addEventListener("visibilitychange", wake);
    instance.wake();
    return () => {
      instance.stop();
      if (monitor.current === instance) monitor.current = null;
      window.removeEventListener("focus", wake);
      document.removeEventListener("visibilitychange", wake);
    };
  }, [options.configDir, accept]);

  useEffect(() => { if (options.canCheck) monitor.current?.wake(); }, [options.canCheck, options.canNotify, modalOpen]);

  const dismiss = useCallback((hint = false) => {
    setNotice(null); noticeTime.current = null;
    if (hint) runtime.current.onHint(runtime.current.lang === "zh"
      ? "稍后可到「设置 → 通用设置 → 环境与配置检查」处理。"
      : "You can check and repair this later in Settings → General → Environment & configuration check.");
  }, []);

  const noticeVisible = Boolean(notice && report?.status === "issues"
    && configHealthIssueKey(notice) === configHealthIssueKey(report))
    && options.canNotify && visible && !modalOpen;

  useEffect(() => {
    if (!notice || !noticeVisible || repairing || checking) return;
    const issue = configHealthIssueKey(notice);
    if (noticeTime.current?.issue !== issue) noticeTime.current = { issue, remaining: 30_000 };
    const started = Date.now();
    const timer = window.setTimeout(() => dismiss(true), noticeTime.current.remaining);
    return () => {
      window.clearTimeout(timer);
      if (noticeTime.current?.issue === issue) noticeTime.current.remaining = Math.max(0, noticeTime.current.remaining - (Date.now() - started));
    };
  }, [notice, noticeVisible, repairing, checking, dismiss]);

  const check = useCallback(async () => {
    if (busyRef.current || !runtime.current.canCheck) return;
    const directory = scope.current;
    const request = ++revision.current;
    busyRef.current = true; setChecking(true); setError("");
    const current = () => mounted.current && scope.current === directory && request === revision.current;
    try {
      const next = await invoke<ConfigHealthReport>("check_codex_config", { configDir: directory || null });
      if (current()) {
        accept(next);
        if (next.status === "issues") registry.current?.mark(next);
        if (next.status === "healthy") { registry.current?.clear(next); dismiss(); }
      }
    } catch (cause) { if (current()) setError(String(cause)); }
    finally { if (current()) { busyRef.current = false; setChecking(false); } }
  }, [accept, dismiss]);

  const repair = useCallback(async () => {
    const before = reportRef.current ?? noticeRef.current;
    if (!before?.canRepair || busyRef.current || !runtime.current.canCheck) return;
    const directory = scope.current;
    const request = ++revision.current;
    busyRef.current = true; setRepairing(true); setError("");
    const current = () => mounted.current && scope.current === directory && request === revision.current;
    try {
      const result = await invoke<ConfigHealthRepairResult>("repair_codex_config", { configDir: before.codexDir, expectedFingerprint: before.fingerprint });
      if (!current()) return;
      dismiss(); accept(result.report);
      if (result.report.status === "healthy") registry.current?.clear(result.report);
      const zh = runtime.current.lang === "zh";
      runtime.current.onHint(result.report.status === "healthy"
        ? (zh ? "配置已检查并修复，请重新打开原来的 Codex 对话。" : "Configuration checked and repaired. Reopen the Codex conversation.")
        : (zh ? "可自动处理的部分已修复，其余问题可在通用设置中查看。" : "Supported repairs are complete. Review the remaining issues in General settings."));
      runtime.current.onRepaired();
    } catch (cause) {
      if (current()) {
        dismiss(); setError(String(cause));
        runtime.current.onHint(runtime.current.lang === "zh" ? "配置未修复，请到「设置 → 通用设置」查看原因或重新检查。" : "Configuration was not repaired. Review the issue or check again in Settings → General.");
      }
    } finally { if (current()) { busyRef.current = false; setRepairing(false); } }
  }, [accept, dismiss]);

  const openConfig = useCallback(async () => {
    try { await invoke("open_codex_config_file", { configDir: reportRef.current?.codexDir || scope.current || null }); }
    catch (cause) { if (mounted.current) setError(String(cause)); }
  }, []);

  return { report, notice, checking, repairing, error, check, repair, dismiss, openConfig,
    noticeVisible };
}
