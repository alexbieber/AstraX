import test from "node:test";
import assert from "node:assert/strict";
import { getOfficialPlan } from "../src/officialPlan.ts";

test("Pro and Pro Lite aliases keep distinct names and badge styles", () => {
  assert.deepEqual(getOfficialPlan("pro", "zh"), { label: "Pro 20x", tone: "pro20" });
  for (const value of ["prolite", "pro-lite", "PRO_LITE", " pro_lite "]) {
    assert.deepEqual(getOfficialPlan(value, "zh"), { label: "Pro 5x", tone: "pro5" });
  }
  assert.equal(getOfficialPlan("plus", "en").label, "Plus");
  assert.equal(getOfficialPlan("team", "zh").label, "Team");
});

test("missing or invalid plans stay unknown and future plans do not claim a premium tier", () => {
  for (const value of [undefined, null, "", "  ", "pro\n", "pro\n<script>", "x".repeat(65)]) {
    assert.equal(getOfficialPlan(value, "zh"), null);
  }
  assert.deepEqual(getOfficialPlan("Future_Plan-2", "en"), { label: "Future_Plan-2", tone: "neutral" });
  assert.deepEqual(getOfficialPlan("constructor", "en"), { label: "constructor", tone: "neutral" });
});
