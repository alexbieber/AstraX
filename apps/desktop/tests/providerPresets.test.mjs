import assert from "node:assert/strict";
import test from "node:test";
import { createPresetProvider, PROVIDER_PRESETS } from "../src/providerPresets.ts";

test("region and plan changes create independent credentials and endpoint settings", () => {
  const china = createPresetProvider("minimax", "cn");
  china.apiKey = "synthetic-china-only";
  china.tomlConfig = "synthetic old authentication";
  const global = createPresetProvider("minimax", "global");
  assert.equal(global.baseUrl, "https://api.minimax.io/v1");
  assert.equal(global.apiKey, "");
  assert.equal(global.tomlConfig, "");
  const metered = createPresetProvider("mimo", "api");
  metered.apiKey = "synthetic-metered-only";
  const plan = createPresetProvider("mimo", "token-plan");
  assert.equal(plan.baseUrl, "https://token-plan-cn.xiaomimimo.com/v1");
  assert.equal(plan.apiKey, "");
  assert.notEqual(plan.id, metered.id);
});

test("editing an instantiated model mapping cannot change future presets", () => {
  const edited = createPresetProvider("deepseek", "default");
  edited.modelMappings[0].model = "user-custom-model";
  edited.modelMappings[0].contextWindow = 12345;
  const fresh = createPresetProvider("deepseek", "default");
  assert.equal(fresh.modelMappings[0].model, "deepseek-v4-flash");
  assert.equal(fresh.modelMappings[0].contextWindow, 1048576);
});

test("every preset and plan inherits the custom form's full TOML before provider fields are applied", () => {
  const customToml = `# existing user configuration
model = "existing-model"
model_provider = "custom"
model_reasoning_effort = "high"
approval_policy = "on-request"
[model_providers.custom]
name = "Existing provider"
base_url = "https://existing.example.test/v1"
[mcp_servers.docs]
command = "docs-server"
[features]
shell_snapshot = true
[projects."/fixture/project"]
trust_level = "trusted"
`;
  for (const preset of PROVIDER_PRESETS) {
    for (const variant of preset.variants) {
      const created = createPresetProvider(preset.id, variant.id, customToml);
      assert.equal(created.tomlConfig, customToml, `${preset.id}/${variant.id} lost inherited settings`);
      assert.equal(created.baseUrl, variant.baseUrl);
      assert.equal(created.model, variant.models[0].model);
      assert.equal(created.apiKey, "");
    }
  }
});

test("preset menus contain their real default model and use native Responses routes", () => {
  for (const preset of PROVIDER_PRESETS) {
    for (const variant of preset.variants) {
      const provider = createPresetProvider(preset.id, variant.id);
      assert.equal(provider.wireApi, "responses");
      assert.equal(provider.requiresOpenaiAuth, false);
      assert.ok(provider.modelMappings.some((row) => row.model === provider.model));
      assert.equal(new Set(provider.modelMappings.map((row) => row.model)).size, provider.modelMappings.length);
      assert.ok(!provider.baseUrl.includes("anthropic"));
      assert.ok(!provider.modelMappings.some((row) => row.model.startsWith("gpt-")));
    }
  }
  assert.deepEqual(createPresetProvider("kimi", "default").modelMappings.map((row) => row.model), ["kimi-k3"]);
  assert.equal(createPresetProvider("official", ""), null);
  assert.equal(createPresetProvider("custom", ""), null);
});
