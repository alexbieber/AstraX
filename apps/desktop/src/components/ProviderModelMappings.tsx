import { useEffect, useId, useMemo, useState } from "react";
import { ChevronDown, Download, ListFilter, Plus, Trash2 } from "lucide-react";
import type { ProviderModelMapping } from "../types";

type Language = "zh" | "en";
type MappingError = { model?: string; contextWindow?: string };
const MAX_MAPPINGS = 64;
const MAX_CONTEXT_WINDOW = 10_000_000;

function getCopy(lang: Language) {
  return lang === "zh" ? {
    title: "模型映射", optional: "可选", subtitle: "让 Codex 的模型菜单显示这个供应商实际提供的模型。",
    hint: "适用于支持 Responses 接口的模型。保存并启用后，重启 Codex 更新模型菜单。已有会话会保留原来的模型选择。",
    displayName: "菜单显示名称", displayPlaceholder: "留空使用模型 ID", model: "实际模型 ID", modelPlaceholder: "填写供应商提供的模型 ID",
    context: "上下文窗口", contextPlaceholder: "可选，如 128000", contextHint: "单位为 Token；留空使用默认值，最大 10,000,000。",
    addCurrent: "添加当前模型", importModels: "导入已获取模型", addRow: "添加空行", remove: "删除映射",
    empty: "暂未设置模型映射", emptyHint: "留空表示不设置自定义模型菜单。可以添加当前模型，或先获取供应商的模型列表再导入。",
    noCurrent: "请先填写上方的模型", currentExists: "当前模型已添加", fetchFirst: "请先在上方获取模型列表", allImported: "已获取的模型均已添加",
    limit: "模型菜单最多支持 64 个模型（包含当前默认模型）。", reserved: "当前默认模型会自动保留在菜单中，并计入 64 个模型的上限。",
    missingModel: "请填写实际模型 ID", invalidModel: "模型 ID 不能包含控制字符", longModel: "模型 ID 最多 200 个字符",
    invalidContext: "请填写 1 至 10,000,000 的整数，或留空。", fixErrors: "请先修正标红的内容，再保存供应商。",
    rowLabel: (index: number) => `第 ${index + 1} 行`, imported: (count: number) => `已添加 ${count} 个模型。`,
    importLimited: (added: number, skipped: number) => `已添加 ${added} 个模型，达到 64 个上限，另有 ${skipped} 个未添加。`,
    invalidDefault: "上方的当前模型 ID 不符合要求，请先修改（最多 200 个字符，不能包含控制字符）。",
  } : {
    title: "Model mappings", optional: "Optional", subtitle: "Show this provider’s actual models in the Codex model menu.",
    hint: "For models that support the Responses API. Save and enable this provider, then restart Codex to update its model menu. Existing conversations keep their selected model.",
    displayName: "Menu display name", displayPlaceholder: "Defaults to the model ID", model: "Actual model ID", modelPlaceholder: "Enter the provider’s model ID",
    context: "Context window", contextPlaceholder: "Optional, e.g. 128000", contextHint: "In tokens. Leave blank for the default; maximum 10,000,000.",
    addCurrent: "Add current model", importModels: "Import fetched models", addRow: "Add row", remove: "Remove mapping",
    empty: "No model mappings yet", emptyHint: "Leave this empty to avoid setting a custom model menu. Add the current model, or fetch the provider’s models above and import them.",
    noCurrent: "Enter a model above first", currentExists: "The current model is already included", fetchFirst: "Fetch the model list above first", allImported: "All fetched models are already included",
    limit: "The model menu supports up to 64 models, including the current default model.", reserved: "The current default model is kept in the menu automatically and counts toward the 64-model limit.",
    missingModel: "Enter the actual model ID", invalidModel: "Model IDs cannot contain control characters", longModel: "Model IDs cannot exceed 200 characters",
    invalidContext: "Enter a whole number from 1 to 10,000,000, or leave blank.", fixErrors: "Correct the highlighted fields before saving this provider.",
    rowLabel: (index: number) => `Row ${index + 1}`, imported: (count: number) => `Added ${count} model(s).`,
    importLimited: (added: number, skipped: number) => `Added ${added} model(s). The 64-model limit was reached; ${skipped} were not added.`,
    invalidDefault: "Correct the current model ID above first: at most 200 characters, without control characters.",
  };
}

function hasInvalidModelId(value: string): boolean {
  return Array.from(value).length > 200 || /[\u0000-\u001f\u007f-\u009f]/.test(value);
}

function menuSize(rows: readonly ProviderModelMapping[], currentModel: string): number {
  const current = currentModel.trim();
  return rows.length + (rows.length > 0 && current && !rows.some((row) => row.model.trim() === current) ? 1 : 0);
}

export function validateProviderModelMappings(rows: readonly ProviderModelMapping[], currentModel: string, lang: Language) {
  const copy = getCopy(lang);
  const counts = new Map<string, number>();
  for (const row of rows) {
    const id = row.model.trim();
    if (id) counts.set(id, (counts.get(id) ?? 0) + 1);
  }
  const errors: MappingError[] = rows.map((row) => {
    const id = row.model.trim();
    const error: MappingError = {};
    if (!id) error.model = copy.missingModel;
    else if (/[\u0000-\u001f\u007f-\u009f]/.test(id)) error.model = copy.invalidModel;
    else if (Array.from(id).length > 200) error.model = copy.longModel;
    else if ((counts.get(id) ?? 0) > 1) error.model = lang === "zh" ? "此模型 ID 已添加，请删除重复行。" : "This model ID is already included. Remove the duplicate row.";
    if (row.contextWindow !== null && (!Number.isSafeInteger(row.contextWindow) || row.contextWindow <= 0 || row.contextWindow > MAX_CONTEXT_WINDOW)) error.contextWindow = copy.invalidContext;
    return error;
  });
  const size = menuSize(rows, currentModel);
  const defaultInvalid = rows.length > 0 && hasInvalidModelId(currentModel.trim());
  return { errors, size, valid: size <= MAX_MAPPINGS && !defaultInvalid && errors.every((error) => !error.model && !error.contextWindow), defaultInvalid };
}

export function ProviderModelMappings({ lang, rows, currentModel, availableModels, disabled, onChange }: {
  lang: Language;
  rows: readonly ProviderModelMapping[];
  currentModel: string;
  availableModels: readonly string[];
  disabled: boolean;
  onChange: (rows: ProviderModelMapping[]) => void;
}) {
  const copy = getCopy(lang);
  const id = useId();
  const [expanded, setExpanded] = useState(rows.length > 0);
  const [notice, setNotice] = useState("");
  // Keep incomplete numeric input visible. Its parent value is the invalid
  // sentinel 0, so both the form and save validation reject it instead of
  // silently serializing it as an omitted optional value.
  const [contextDrafts, setContextDrafts] = useState<Record<number, { value: number | null; text: string }>>({});
  const validation = validateProviderModelMappings(rows, currentModel, lang);
  const current = currentModel.trim();
  const currentIncluded = Boolean(current) && rows.some((row) => row.model.trim() === current);
  const candidates = useMemo(() => {
    const existing = new Set(rows.map((row) => row.model.trim()));
    return Array.from(new Set(availableModels.map((model) => model.trim()).filter(Boolean))).filter((model) => !existing.has(model));
  }, [availableModels, rows]);
  const canAppend = (model: string) => menuSize([...rows, { model, displayName: model, contextWindow: null }], current) <= MAX_MAPPINGS;
  const canAddCurrent = Boolean(current) && !currentIncluded && !hasInvalidModelId(current) && canAppend(current);
  const canImport = candidates.some(canAppend);
  const reserved = rows.length > 0 && Boolean(current) && !currentIncluded;

  useEffect(() => { if (rows.length > 0) setExpanded(true); }, [rows.length]);

  const update = (index: number, patch: Partial<ProviderModelMapping>) => {
    setNotice("");
    onChange(rows.map((row, position) => position === index ? { ...row, ...patch } : row));
  };
  const add = (model: string) => {
    if (disabled || !canAppend(model)) return;
    setNotice("");
    setExpanded(true);
    onChange([...rows, { model, displayName: model, contextWindow: null }]);
  };
  const remove = (index: number) => {
    if (disabled) return;
    setContextDrafts((previous) => Object.fromEntries(Object.entries(previous).filter(([key]) => Number(key) !== index).map(([key, value]) => [Number(key) > index ? Number(key) - 1 : Number(key), value])));
    setNotice("");
    onChange(rows.filter((_, position) => position !== index));
  };
  const importModels = () => {
    if (disabled) return;
    let next = [...rows];
    let added = 0;
    let skipped = 0;
    for (const model of candidates) {
      const proposed = [...next, { model, displayName: model, contextWindow: null }];
      if (menuSize(proposed, current) > MAX_MAPPINGS) { skipped += 1; continue; }
      next = proposed;
      added += 1;
    }
    if (added) onChange(next);
    setExpanded(true);
    setNotice(skipped ? copy.importLimited(added, skipped) : copy.imported(added));
  };

  return <details className="cx-provider-mappings" open={expanded} onToggle={(event) => setExpanded(event.currentTarget.open)}>
    <summary><ListFilter size={17} aria-hidden="true" /><div><strong>{copy.title}<span>{copy.optional}</span></strong><p>{copy.subtitle}</p></div><span className={`cx-provider-mappings-count${!validation.valid ? " cx-provider-mappings-count--invalid" : ""}`} title={copy.limit}>{validation.size} / {MAX_MAPPINGS}</span><ChevronDown size={16} className="cx-provider-mappings-chevron" aria-hidden="true" /></summary>
    <div className="cx-provider-mappings-body">
      <p className="cx-provider-mappings-hint">{copy.hint}</p>
      <div className="cx-provider-mappings-toolbar">
        <button type="button" className="cx-providers-button cx-providers-button--secondary cx-providers-button--small" onClick={() => add(current)} disabled={disabled || !canAddCurrent} title={currentIncluded ? copy.currentExists : !current ? copy.noCurrent : hasInvalidModelId(current) ? copy.invalidDefault : !canAddCurrent ? copy.limit : undefined}><Plus size={14} aria-hidden="true" />{copy.addCurrent}</button>
        <button type="button" className="cx-providers-button cx-providers-button--secondary cx-providers-button--small" onClick={importModels} disabled={disabled || !canImport} title={!availableModels.length ? copy.fetchFirst : !candidates.length ? copy.allImported : !canImport ? copy.limit : undefined}><Download size={14} aria-hidden="true" />{copy.importModels}</button>
        <button type="button" className="cx-providers-button cx-providers-button--secondary cx-providers-button--small" onClick={() => add("")} disabled={disabled || !canAppend("")} title={!canAppend("") ? copy.limit : undefined}><Plus size={14} aria-hidden="true" />{copy.addRow}</button>
      </div>
      {notice && <p className="cx-provider-mappings-notice" role="status">{notice}</p>}
      {rows.length === 0 ? <div className="cx-provider-mappings-empty"><ListFilter size={22} aria-hidden="true" /><strong>{copy.empty}</strong><p>{copy.emptyHint}</p></div> : <>
        <div className="cx-provider-mappings-labels" aria-hidden="true"><span>{copy.displayName}</span><span>{copy.model}</span><span>{copy.context}</span><span /></div>
        <div className="cx-provider-mappings-rows">
          {rows.map((row, index) => {
            const error = validation.errors[index];
            const contextDraft = contextDrafts[index];
            const contextText = contextDraft && Object.is(contextDraft.value, row.contextWindow) ? contextDraft.text : row.contextWindow === null ? "" : String(row.contextWindow);
            return <div className="cx-provider-mapping-row" key={index} role="group" aria-label={copy.rowLabel(index)}>
              <label className="cx-provider-mapping-field"><span>{copy.displayName}</span><input value={row.displayName} placeholder={row.model.trim() || copy.displayPlaceholder} aria-label={`${copy.displayName} · ${copy.rowLabel(index)}`} onChange={(event) => update(index, { displayName: event.currentTarget.value })} disabled={disabled} /></label>
              <label className="cx-provider-mapping-field"><span>{copy.model}</span><input value={row.model} placeholder={copy.modelPlaceholder} aria-label={`${copy.model} · ${copy.rowLabel(index)}`} aria-invalid={Boolean(error.model)} aria-describedby={error.model ? `${id}-${index}-model-error` : undefined} onChange={(event) => update(index, { model: event.currentTarget.value })} disabled={disabled} spellCheck={false} />{error.model && <small id={`${id}-${index}-model-error`}>{error.model}</small>}</label>
              <label className="cx-provider-mapping-field"><span>{copy.context}</span><input value={contextText} inputMode="numeric" placeholder={copy.contextPlaceholder} aria-label={`${copy.context} · ${copy.rowLabel(index)}`} aria-invalid={Boolean(error.contextWindow)} aria-describedby={error.contextWindow ? `${id}-${index}-context-error` : `${id}-context-hint`} onChange={(event) => {
                const text = event.currentTarget.value;
                const trimmed = text.trim();
                const number = Number(trimmed);
                const value = !trimmed ? null : /^\d+$/.test(trimmed) && Number.isSafeInteger(number) && number > 0 && number <= MAX_CONTEXT_WINDOW ? number : 0;
                setContextDrafts((previous) => ({ ...previous, [index]: { text, value } }));
                update(index, { contextWindow: value });
              }} disabled={disabled} />{error.contextWindow && <small id={`${id}-${index}-context-error`}>{error.contextWindow}</small>}</label>
              <button type="button" className="cx-providers-icon-button cx-providers-icon-button--danger cx-provider-mapping-remove" onClick={() => remove(index)} disabled={disabled} title={`${copy.remove} · ${copy.rowLabel(index)}`} aria-label={`${copy.remove} · ${copy.rowLabel(index)}`}><Trash2 size={14} aria-hidden="true" /></button>
            </div>;
          })}
        </div>
        <p className="cx-provider-mappings-field-hint" id={`${id}-context-hint`}>{copy.contextHint}</p>
        {reserved && <p className="cx-provider-mappings-field-hint">{copy.reserved}</p>}
        {!validation.valid && <p className="cx-provider-mappings-error" role="status">{validation.size > MAX_MAPPINGS ? copy.limit : validation.defaultInvalid ? copy.invalidDefault : copy.fixErrors}</p>}
      </>}
    </div>
  </details>;
}
