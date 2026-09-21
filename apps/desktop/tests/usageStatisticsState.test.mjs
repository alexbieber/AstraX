import assert from "node:assert/strict";
import test from "node:test";
import { initialUsageLoadState, sameUsageQuery, usageLoadReducer } from "../src/usageStatisticsState.ts";

const query = (range = "7d", model = "", configDir = "/codex/a") => ({ configDir, range, model });
const result = (totalTokens) => ({ totals: { totalTokens }, availableModels: ["deepseek-v4-pro"] });

function loaded(queryValue = query()) {
  let state = usageLoadReducer(initialUsageLoadState, { type: "start", requestId: 1, query: queryValue });
  return usageLoadReducer(state, { type: "success", requestId: 1, data: result(700) });
}

test("changing date range retains the mounted data and its original range until replacement arrives", () => {
  const previous = loaded();
  let state = usageLoadReducer(previous, { type: "start", requestId: 2, query: query("today") });
  assert.equal(state.record, previous.record);
  assert.equal(state.busy, true);
  assert.equal(state.record.query.range, "7d");
  assert.equal(sameUsageQuery(state.record.query, state.query), false);
  state = usageLoadReducer(state, { type: "success", requestId: 2, data: result(100) });
  assert.equal(state.record.query.range, "today");
  assert.equal(state.record.data.totals.totalTokens, 100);
  assert.equal(state.busy, false);
});

test("rapid range selections cannot be overwritten by slower earlier responses or errors", () => {
  let state = loaded();
  state = usageLoadReducer(state, { type: "start", requestId: 2, query: query("today") });
  state = usageLoadReducer(state, { type: "start", requestId: 3, query: query("30d") });
  state = usageLoadReducer(state, { type: "start", requestId: 4, query: query("all") });
  state = usageLoadReducer(state, { type: "success", requestId: 4, data: result(9_000) });
  const latest = state;
  state = usageLoadReducer(state, { type: "success", requestId: 2, data: result(100) });
  state = usageLoadReducer(state, { type: "failure", requestId: 3, error: "old read failed" });
  assert.equal(state, latest);
  assert.equal(state.record.query.range, "all");
  assert.equal(state.record.data.totals.totalTokens, 9_000);
  assert.equal(state.error, "");
});

test("a failed new filter retains explicitly identifiable previous results and retry clears the error", () => {
  const previous = loaded();
  let state = usageLoadReducer(previous, { type: "start", requestId: 2, query: query("today", "deepseek-v4-pro") });
  state = usageLoadReducer(state, { type: "failure", requestId: 2, error: "temporarily unavailable" });
  assert.equal(state.record, previous.record);
  assert.equal(state.record.query.model, "");
  assert.equal(state.query.model, "deepseek-v4-pro");
  assert.equal(state.busy, false);
  assert.equal(state.error, "temporarily unavailable");
  state = usageLoadReducer(state, { type: "start", requestId: 3, query: state.query });
  assert.equal(state.error, "");
  assert.equal(state.record, previous.record);
});

test("changing Codex directory cannot show another directory's usage or accept its pending request", () => {
  let state = loaded();
  state = usageLoadReducer(state, { type: "start", requestId: 2, query: query("all") });
  state = usageLoadReducer(state, { type: "start", requestId: 3, query: query("all", "", "/codex/b") });
  assert.equal(state.record, null);
  state = usageLoadReducer(state, { type: "success", requestId: 2, data: result(9_000) });
  assert.equal(state.record, null);
  assert.equal(state.busy, true);
  state = usageLoadReducer(state, { type: "success", requestId: 3, data: result(20) });
  assert.equal(state.record.query.configDir, "/codex/b");
  assert.equal(state.record.data.totals.totalTokens, 20);
});

test("leaving the tab invalidates in-flight reads while keeping data available when returning", () => {
  let state = loaded();
  const record = state.record;
  state = usageLoadReducer(state, { type: "start", requestId: 2, query: query() });
  state = usageLoadReducer(state, { type: "cancel", requestId: 3 });
  state = usageLoadReducer(state, { type: "success", requestId: 2, data: result(999) });
  assert.equal(state.record, record);
  assert.equal(state.busy, false);
  state = usageLoadReducer(state, { type: "start", requestId: 4, query: query() });
  assert.equal(state.record, record);
  state = usageLoadReducer(state, { type: "success", requestId: 4, data: result(800) });
  assert.equal(state.record.data.totals.totalTokens, 800);
});
