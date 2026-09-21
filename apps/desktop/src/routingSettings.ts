// Parameter ranges and save/reset semantics adapted from CC Switch (MIT).
// Copyright (c) 2025 Jason Young. See THIRD_PARTY_NOTICES.md.
export const routingFields = [
  { key: "maxRetries", min: 0, max: 10, value: 3, group: "retry", zh: "最大重试次数", en: "Maximum retries", hintZh: "失败后最多再尝试几家供应商，0 表示不重试。", hintEn: "Additional providers to try after a failure. 0 disables retries." },
  { key: "circuitFailureThreshold", min: 1, max: 20, value: 4, group: "retry", zh: "连续失败阈值", en: "Consecutive failures", hintZh: "连续失败达到此次数后，暂时跳过该供应商。", hintEn: "Temporarily skip a provider after this many consecutive failures." },
  { key: "streamingFirstByteTimeout", min: 1, max: 120, value: 60, group: "timeout", zh: "流式首字节超时", en: "First response timeout", hintZh: "等待开始返回内容的时间，单位为秒。", hintEn: "Seconds to wait for the first response data." },
  { key: "streamingIdleTimeout", min: 0, max: 600, value: 120, group: "timeout", zh: "流式静默超时", en: "Stream idle timeout", hintZh: "回复过程中等待下一段内容的秒数；0 表示不限制。", hintEn: "Seconds between response chunks. 0 disables this timeout." },
  { key: "nonStreamingTimeout", min: 60, max: 1200, value: 600, group: "timeout", zh: "非流式总超时", en: "Non-streaming timeout", hintZh: "等待完整回复的最长时间，单位为秒。", hintEn: "Total seconds to wait for a complete non-streaming response." },
  { key: "circuitSuccessThreshold", min: 1, max: 10, value: 2, group: "recovery", zh: "恢复成功次数", en: "Recovery successes", hintZh: "试探恢复时，连续成功几次后恢复正常。", hintEn: "Successful trial requests required to restore normal routing." },
  { key: "circuitTimeoutSeconds", min: 0, max: 300, value: 60, group: "recovery", zh: "恢复等待时间", en: "Recovery wait", hintZh: "暂时跳过后，隔多少秒再尝试。", hintEn: "Seconds to wait before trying an unavailable provider again." },
  { key: "circuitErrorRateThreshold", min: 0, max: 100, value: 60, group: "recovery", zh: "错误率阈值 (%)", en: "Error rate threshold (%)", hintZh: "达到此比例时，也会暂时跳过供应商。", hintEn: "Also skip a provider when its failure rate reaches this percentage." },
  { key: "circuitMinRequests", min: 5, max: 100, value: 10, group: "recovery", zh: "最小请求数", en: "Minimum requests", hintZh: "累计达到此请求数后，才开始判断错误率。", hintEn: "Requests required before evaluating the error rate." },
] as const;

export type RoutingField = typeof routingFields[number]["key"];
export type RoutingSettings = Record<RoutingField, number> & {
  version: number; routerEnabled: boolean; takeoverEnabled: boolean; autoFailoverEnabled: boolean;
  listenAddress: string; listenPort: number; providerIds: string[];
};
export type RoutingDraft = Omit<RoutingSettings, RoutingField | "listenPort"> & Record<RoutingField | "listenPort", string>;

export function routingDraft(settings: RoutingSettings): RoutingDraft {
  const values = Object.fromEntries(routingFields.map(({ key }) => [key, String(key === "circuitErrorRateThreshold" ? Number((settings[key] * 100).toFixed(6)) : settings[key])])) as Record<RoutingField, string>;
  return { ...settings, ...values, listenPort: String(settings.listenPort), providerIds: [...settings.providerIds] };
}

export function sameRoutingDraft(a: RoutingDraft, b: RoutingDraft): boolean {
  return a.routerEnabled === b.routerEnabled && a.takeoverEnabled === b.takeoverEnabled
    && a.autoFailoverEnabled === b.autoFailoverEnabled && a.listenAddress === b.listenAddress && a.listenPort === b.listenPort
    && routingFields.every(({ key }) => a[key] === b[key])
    && a.providerIds.length === b.providerIds.length && a.providerIds.every((id, index) => id === b.providerIds[index]);
}

export function parseRoutingDraft(draft: RoutingDraft): { settings: RoutingSettings | null; errors: Partial<Record<RoutingField | "listenAddress" | "listenPort" | "providerIds", string>> } {
  const errors: ReturnType<typeof parseRoutingDraft>["errors"] = {};
  const numbers = {} as Record<RoutingField, number>;
  for (const field of routingFields) {
    const text = draft[field.key].trim();
    const pattern = field.key === "circuitErrorRateThreshold" ? /^\d+(?:\.\d+)?$/ : /^\d+$/;
    const value = Number(text);
    if (!pattern.test(text) || !Number.isFinite(value) || value < field.min || value > field.max) errors[field.key] = `${field.min}–${field.max}`;
    numbers[field.key] = field.key === "circuitErrorRateThreshold" ? value / 100 : value;
  }
  let address = draft.listenAddress.trim();
  if (address.toLowerCase() === "localhost") address = "127.0.0.1";
  const ipv4 = /^(\d{1,3}\.){3}\d{1,3}$/.test(address) && address.split(".").every((part) => Number(part) <= 255 && String(Number(part)) === part);
  let ipv6 = false;
  if (address.includes(":")) {
    try { ipv6 = new URL(`http://[${address}]/`).hostname.startsWith("["); } catch { /* invalid literal */ }
  }
  if (!ipv4 && !ipv6) errors.listenAddress = "IPv4 / IPv6 / localhost";
  const port = Number(draft.listenPort);
  if (!/^\d+$/.test(draft.listenPort.trim()) || port < 1024 || port > 65535) errors.listenPort = "1024–65535";
  if (draft.providerIds.length > 64 || new Set(draft.providerIds).size !== draft.providerIds.length) errors.providerIds = "1–64";
  return { errors, settings: Object.keys(errors).length ? null : {
    ...draft, ...numbers, version: 2, listenAddress: address, listenPort: port, providerIds: [...draft.providerIds],
  } };
}
