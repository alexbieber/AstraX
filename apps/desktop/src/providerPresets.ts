import type { ProviderPresetChoice } from "./components/ProviderPresetPicker";
import type { ProviderModelMapping, SavedProvider } from "./types";

export type ProviderPresetVariant = {
  id: string;
  label: string;
  labelEn: string;
  providerName: string;
  baseUrl: string;
  models: readonly ProviderModelMapping[];
  note: string;
  noteEn: string;
};

export type ProviderPreset = ProviderPresetChoice & {
  variants: readonly ProviderPresetVariant[];
  sources?: readonly string[];
};

const model = (id: string, displayName: string, contextWindow: number | null): ProviderModelMapping => ({ model: id, displayName, contextWindow });
const deepseekModels = [model("deepseek-v4-flash", "DeepSeek V4 Flash", 1048576), model("deepseek-v4-pro", "DeepSeek V4 Pro", 1048576)];
const minimaxModels = [model("MiniMax-M3", "MiniMax M3", 1000000), model("MiniMax-M2.7", "MiniMax M2.7", 204800)];
const mimoModels = [model("mimo-v2.5-pro", "MiMo V2.5 Pro", 1048576), model("mimo-v2.5", "MiMo V2.5", 1048576)];

// Reviewed against vendor documentation on 2026-09-09. Only native Responses
// endpoints belong here; Chat/Anthropic-only plans need a separate adapter.
export const PROVIDER_PRESETS: readonly ProviderPreset[] = [
  { id: "custom", name: "自定义配置", nameEn: "Custom", brand: "custom", subtitle: "自行填写 API 配置", subtitleEn: "Your own API settings", variants: [] },
  { id: "official", name: "OpenAI Official", brand: "openai", subtitle: "ChatGPT 账号登录", subtitleEn: "Sign in with ChatGPT", variants: [] },
  {
    id: "deepseek", name: "DeepSeek", brand: "deepseek", subtitle: "V4 Flash / Pro",
    variants: [{ id: "default", label: "开放平台", labelEn: "API platform", providerName: "DeepSeek", baseUrl: "https://api.deepseek.com", models: deepseekModels,
      note: "使用 DeepSeek 开放平台的 API Key。", noteEn: "Use an API key from the DeepSeek platform." }],
    sources: ["https://api-docs.deepseek.com/quick_start/agent_integrations/codex"],
  },
  {
    id: "minimax", name: "MiniMax", brand: "minimax", subtitle: "M3 / M2.7 · 国内与国际", subtitleEn: "M3 / M2.7 · China & global",
    variants: [
      { id: "cn", label: "国内", labelEn: "China", providerName: "MiniMax 国内", baseUrl: "https://api.minimax.cn/v1", models: minimaxModels,
        note: "使用 MiniMax 国内平台的 API Key；请确认账号已开通所选模型。", noteEn: "Use a MiniMax China API key with access to the selected model." },
      { id: "global", label: "国际", labelEn: "Global", providerName: "MiniMax 国际", baseUrl: "https://api.minimax.io/v1", models: minimaxModels,
        note: "使用 MiniMax 国际平台的 API Key；请确认账号已开通所选模型。", noteEn: "Use a MiniMax global API key with access to the selected model." },
    ],
    sources: ["https://platform.minimaxi.com/docs/token-plan/codex", "https://platform.minimax.io/docs/token-plan/codex", "https://platform.minimax.io/docs/api-reference/responses-create"],
  },
  {
    id: "mimo", name: "小米 MiMo", nameEn: "Xiaomi MiMo", brand: "mimo", subtitle: "V2.5 Pro / V2.5",
    variants: [
      { id: "api", label: "按量付费", labelEn: "Pay as you go", providerName: "小米 MiMo", baseUrl: "https://api.xiaomimimo.com/v1", models: mimoModels,
        note: "使用按量付费的 sk- 开头 API Key。", noteEn: "Use a pay-as-you-go API key beginning with sk-." },
      { id: "token-plan", label: "Token Plan 套餐", labelEn: "Token Plan", providerName: "小米 MiMo Token Plan", baseUrl: "https://token-plan-cn.xiaomimimo.com/v1", models: mimoModels,
        note: "使用 Token Plan 套餐的 tp- 开头 API Key，套餐与按量付费地址不同。", noteEn: "Use a Token Plan key beginning with tp-. This plan has its own API endpoint." },
    ],
    sources: ["https://mimo.mi.com/docs/zh-CN/tokenplan/integration/codex-configuration"],
  },
  {
    id: "kimi", name: "Kimi", brand: "kimi", subtitle: "K3 · 开放平台", subtitleEn: "K3 · API platform",
    variants: [{ id: "default", label: "开放平台", labelEn: "API platform", providerName: "Kimi", baseUrl: "https://api.moonshot.cn/v1", models: [model("kimi-k3", "Kimi K3", 1048576)],
      note: "使用 Kimi 开放平台的 API Key，不适用于 Kimi Code 订阅密钥。", noteEn: "Use a Kimi API platform key. Kimi Code subscription keys use a different service." }],
    sources: ["https://platform.kimi.com/docs/guide/codex-kimi", "https://platform.kimi.com/docs/api/responses"],
  },
  {
    id: "glm", name: "智谱 GLM", nameEn: "Zhipu GLM", brand: "glm", subtitle: "GLM Coding Plan",
    variants: [{ id: "cn", label: "Coding Plan", labelEn: "Coding Plan", providerName: "智谱 GLM", baseUrl: "https://open.bigmodel.cn/api/v1", models: [model("glm-5.3", "GLM-5.3", 1048576), model("glm-5-turbo", "GLM-5 Turbo", 204800)],
      note: "使用对应 GLM Coding Plan 套餐的 API Key；团队套餐请使用团队密钥。", noteEn: "Use the API key for your GLM Coding Plan. Team plans require their team key." }],
    sources: ["https://docs.bigmodel.cn/cn/coding-plan/tool/codex"],
  },
  {
    id: "qwen", name: "阿里千问", nameEn: "Alibaba Qwen", brand: "qwen", subtitle: "Qwen3.8 Max / Flash",
    variants: [{ id: "beijing", label: "北京地域", labelEn: "Beijing region", providerName: "阿里千问", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", models: [model("qwen3.8-max", "Qwen3.8 Max", null), model("qwen3.8-flash", "Qwen3.8 Flash", null)],
      note: "使用阿里云百炼北京地域的普通 API Key，不适用于 Coding Plan 专属密钥。", noteEn: "Use a regular Model Studio API key for Beijing. Coding Plan keys use a different endpoint." }],
    sources: ["https://help.aliyun.com/zh/model-studio/compatibility-with-openai-responses-api"],
  },
];

export function getProviderPreset(id: string): ProviderPreset | undefined {
  return PROVIDER_PRESETS.find((preset) => preset.id === id);
}

export function getProviderPresetVariant(presetId: string, variantId: string): ProviderPresetVariant | undefined {
  const preset = getProviderPreset(presetId);
  return preset?.variants.find((variant) => variant.id === variantId) ?? preset?.variants[0];
}

export function createPresetProvider(presetId: string, variantId: string, inheritedToml = ""): SavedProvider | null {
  const variant = getProviderPresetVariant(presetId, variantId);
  if (!variant) return null;
  return {
    id: `${presetId}-${variant.id}`,
    providerName: variant.providerName,
    baseUrl: variant.baseUrl,
    model: variant.models[0].model,
    apiKey: "",
    tomlConfig: inheritedToml,
    wireApi: "responses",
    requiresOpenaiAuth: false,
    modelMappings: variant.models.map((entry) => ({ ...entry })),
  };
}
