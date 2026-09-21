// Parameter ranges and save/reset semantics adapted from CC Switch (MIT).
// Copyright (c) 2025 Jason Young. See THIRD_PARTY_NOTICES.md.
export const routingFields = [
  { key: "maxRetries", min: 0, max: 10, value: 3, group: "retry", zh: "Maximum retries", en: "Maximum retries", hintZh: "Additional providers to try after a failure. 0 disables retries.", hintEn: "Additional providers to try after a failure. 0 disables retries." },
  { key: "circuitFailureThreshold", min: 1, max: 20, value: 4, group: "retry", zh: "Consecutive failures", en: "Consecutive failures", hintZh: "Temporarily skip a provider after this many consecutive failures.", hintEn: "Temporarily skip a provider after this many consecutive failures." },
  { key: "streamingFirstByteTimeout", min: 1, max: 120, value: 60, group: "timeout", zh: "First response timeout", en: "First response timeout", hintZh: "Seconds to wait for the first response data.", hintEn: "Seconds to wait for the first response data." },
  { key: "streamingIdleTimeout", min: 0, max: 600, value: 120, group: "timeout", zh: "Stream idle timeout", en: "Stream idle timeout", hintZh: "Seconds between response chunks. 0 disables this timeout.", hintEn: "Seconds between response chunks. 0 disables this timeout." },
  { key: "nonStreamingTimeout", min: 60, max: 1200, value: 600, group: "timeout", zh: "Non-streaming timeout", en: "Non-streaming timeout", hintZh: "Total seconds to wait for a complete non-streaming response.", hintEn: "Total seconds to wait for a complete non-streaming response." },
  { key: "circuitSuccessThreshold", min: 1, max: 10, value: 2, group: "recovery", zh: "Recovery successes", en: "Recovery successes", hintZh: "Successful trial requests required to restore normal routing.", hintEn: "Successful trial requests required to restore normal routing." },
  { key: "circuitTimeoutSeconds", min: 0, max: 300, value: 60, group: "recovery", zh: "Recovery wait", en: "Recovery wait", hintZh: "Seconds to wait before trying an unavailable provider again.", hintEn: "Seconds to wait before trying an unavailable provider again." },
  { key: "circuitErrorRateThreshold", min: 0, max: 100, value: 60, group: "recovery", zh: "Error rate threshold (%)", en: "Error rate threshold (%)", hintZh: "Also skip a provider when its failure rate reaches this percentage.", hintEn: "Also skip a provider when its failure rate reaches this percentage." },
  { key: "circuitMinRequests", min: 5, max: 100, value: 10, group: "recovery", zh: "Minimum requests", en: "Minimum requests", hintZh: "Requests required before evaluating the error rate.", hintEn: "Requests required before evaluating the error rate." },
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
