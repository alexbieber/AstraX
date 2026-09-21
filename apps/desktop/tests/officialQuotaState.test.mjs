import assert from "node:assert/strict";
import test from "node:test";
import { initialOfficialQuotaState, loadOfficialQuotaDetails, officialQuotaReducer } from "../src/officialQuotaState.ts";

function deferred() {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
const quota = (profileId = "official-a") => ({ profileId, email: "a@example.test", planType: "team", limits: [], checkedAt: "2026-09-09T00:00:00Z" });
const resets = (availableCount = 3, profileId = "official-a") => ({ profileId, availableCount, checkedAt: "2026-09-09T00:00:00Z" });
const key = (dir = "/codex/a", profileId = "official-a") => JSON.stringify([dir, profileId]);
const tick = () => Promise.resolve();

function harness() {
  let state = initialOfficialQuotaState;
  let sequence = 0;
  const calls = [];
  const dispatch = (action) => { state = officialQuotaReducer(state, action); };
  function start({ queryKey = key(), profileId = "official-a", retainResult = false } = {}) {
    const requestId = ++sequence;
    const quotaRead = deferred();
    const resetRead = deferred();
    dispatch({ type: "start", key: queryKey, requestId, retainResult });
    const done = loadOfficialQuotaDetails({
      key: queryKey, requestId, profileId,
      loadQuota: () => { calls.push("quota"); return quotaRead.promise; },
      loadResetCredits: () => { calls.push("resets"); return resetRead.promise; },
      dispatch, isCurrent: () => sequence === requestId,
      mismatchMessage: "wrong account", invalidCountMessage: "invalid count",
    });
    return { quotaRead, resetRead, done };
  }
  return { start, calls, dispatch, get state() { return state; }, close() { dispatch({ type: "clear", requestId: ++sequence }); } };
}

test("quota and reset reads start together; resets can render while quota is still pending", async () => {
  const view = harness();
  const read = view.start();
  assert.deepEqual(view.calls, ["quota", "resets"]);
  read.resetRead.resolve(resets());
  await tick();
  assert.equal(view.state.resetCredits.data.availableCount, 3);
  assert.equal(view.state.resetCredits.busy, false);
  assert.equal(view.state.quota.data, null);
  assert.equal(view.state.quota.busy, true);
  read.quotaRead.resolve(quota());
  await read.done;
  assert.equal(view.state.quota.busy, false);
});

test("quota renders independently and reset failure remains unknown instead of becoming zero", async () => {
  const view = harness();
  const read = view.start();
  read.quotaRead.resolve(quota());
  await tick();
  assert.equal(view.state.quota.data.planType, "team");
  assert.equal(view.state.quota.busy, false);
  assert.equal(view.state.resetCredits.busy, true);
  read.resetRead.reject(new Error("reset service unavailable"));
  await read.done;
  assert.equal(view.state.resetCredits.data, null);
  assert.equal(view.state.resetCredits.error, "reset service unavailable");
  assert.equal(view.state.quota.error, "");
});

test("quota failure does not suppress a valid zero reset count", async () => {
  const view = harness();
  const read = view.start();
  read.quotaRead.reject(new Error("quota service unavailable"));
  read.resetRead.resolve(resets(0));
  await read.done;
  assert.equal(view.state.quota.error, "quota service unavailable");
  assert.equal(view.state.resetCredits.data.availableCount, 0);
  assert.equal(view.state.resetCredits.error, "");
});

test("each failed refresh retains and marks only its own previous result", async () => {
  const view = harness();
  const first = view.start();
  first.quotaRead.resolve(quota());
  first.resetRead.resolve(resets());
  await first.done;
  const previousQuota = view.state.quota.data;
  const second = view.start({ retainResult: true });
  second.quotaRead.reject(new Error("quota refresh failed"));
  second.resetRead.resolve(resets(2));
  await second.done;
  assert.equal(view.state.quota.data, previousQuota);
  assert.equal(view.state.quota.error, "quota refresh failed");
  assert.equal(view.state.resetCredits.data.availableCount, 2);
  assert.equal(view.state.resetCredits.error, "");
  const third = view.start({ retainResult: true });
  third.quotaRead.resolve({ ...quota(), planType: "pro" });
  third.resetRead.reject(new Error("reset refresh failed"));
  await third.done;
  assert.equal(view.state.quota.data.planType, "pro");
  assert.equal(view.state.quota.error, "");
  assert.equal(view.state.resetCredits.data.availableCount, 2);
  assert.equal(view.state.resetCredits.error, "reset refresh failed");
});

test("closing a dialog discards both late results, including after reopening the same account", async () => {
  const view = harness();
  const old = view.start();
  view.close();
  const current = view.start();
  old.quotaRead.resolve(quota());
  old.resetRead.resolve(resets(99));
  await old.done;
  assert.equal(view.state.quota.data, null);
  assert.equal(view.state.resetCredits.data, null);
  current.quotaRead.resolve(quota());
  current.resetRead.resolve(resets(2));
  await current.done;
  assert.equal(view.state.resetCredits.data.availableCount, 2);
});

test("changing account or directory cannot retain old data or accept its late responses", async () => {
  for (const [queryKey, profileId] of [[key("/codex/a", "official-b"), "official-b"], [key("/codex/b"), "official-a"]]) {
    const view = harness();
    const initial = view.start();
    initial.quotaRead.resolve(quota());
    initial.resetRead.resolve(resets());
    await initial.done;
    const old = view.start({ retainResult: true });
    const current = view.start({ queryKey, profileId, retainResult: true });
    assert.equal(view.state.quota.data, null);
    assert.equal(view.state.resetCredits.data, null);
    old.quotaRead.reject(new Error("stale failure"));
    old.resetRead.resolve(resets(99));
    await old.done;
    assert.equal(view.state.quota.error, "");
    assert.equal(view.state.resetCredits.data, null);
    current.quotaRead.resolve(quota(profileId));
    current.resetRead.resolve(resets(1, profileId));
    await current.done;
    assert.equal(view.state.key, queryKey);
    assert.equal(view.state.resetCredits.data.availableCount, 1);
  }
});

test("mismatched accounts and missing, fractional, or negative counts are errors rather than usable results", async () => {
  for (const invalid of [null, undefined, -1, 1.5, Number.NaN]) {
    const view = harness();
    const read = view.start();
    read.quotaRead.resolve(quota("official-b"));
    read.resetRead.resolve({ ...resets(), availableCount: invalid });
    await read.done;
    assert.equal(view.state.quota.data, null);
    assert.equal(view.state.quota.error, "wrong account");
    assert.equal(view.state.resetCredits.data, null);
    assert.equal(view.state.resetCredits.error, "invalid count");
  }
  const view = harness();
  const read = view.start();
  read.quotaRead.resolve(quota());
  read.resetRead.resolve(resets(5, "official-b"));
  await read.done;
  assert.equal(view.state.resetCredits.data, null);
  assert.equal(view.state.resetCredits.error, "wrong account");
});

test("a synchronous failure starting one endpoint still starts the other endpoint", async () => {
  const calls = [];
  const actions = [];
  await loadOfficialQuotaDetails({
    key: key(), requestId: 1, profileId: "official-a",
    loadQuota: () => { calls.push("quota"); throw new Error("bridge unavailable"); },
    loadResetCredits: () => { calls.push("resets"); return Promise.resolve(resets()); },
    dispatch: (action) => actions.push(action), isCurrent: () => true,
    mismatchMessage: "wrong account", invalidCountMessage: "invalid count",
  });
  assert.deepEqual(calls, ["quota", "resets"]);
  assert.equal(actions.find((action) => action.type === "reset-success").data.availableCount, 3);
});
