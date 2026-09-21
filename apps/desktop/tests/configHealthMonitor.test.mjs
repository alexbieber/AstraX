import assert from "node:assert/strict";
import test from "node:test";
import { createConfigHealthMonitor, createConfigHealthNoticeRegistry } from "../src/configHealthMonitor.ts";

function deferred() {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
const flush = async () => { await Promise.resolve(); await Promise.resolve(); };
const issue = { code: "missing-provider", title: "供应商配置缺失", description: "选中的供应商没有对应配置。", repairable: true };
function report(status = "issues", overrides = {}) {
  return {
    codexDir: "/fixtures/codex", fingerprint: `${status}-file`, status,
    issues: status === "issues" ? [issue] : [], canRepair: status === "issues",
    repairSummary: status === "issues" ? ["恢复选中的供应商配置"] : [],
    checkedAt: "2026-09-10T00:00:00Z", ...overrides,
  };
}

function harness(callbacks = {}) {
  const requests = [], applied = [], problems = [], timers = new Map();
  let timerId = 0;
  const state = { visible: true, readable: true, notifiable: true, revision: 0, now: 0 };
  const monitor = createConfigHealthMonitor({
    read: () => { const request = deferred(); requests.push(request); return request.promise; },
    onReport: (value) => { applied.push(value); callbacks.onReport?.(value); },
    onProblem: (value) => { problems.push(value); callbacks.onProblem?.(value); },
    onRecovered: callbacks.onRecovered,
    revision: () => state.revision,
    isVisible: () => state.visible,
    canRead: () => state.readable,
    canNotify: () => state.notifiable,
    now: () => state.now,
    intervalMs: 20_000,
    settleMs: 1_000,
    schedule: (callback, delay) => {
      const id = ++timerId;
      timers.set(id, { callback, delay, at: state.now + delay });
      return () => timers.delete(id);
    },
  });
  function tick() {
    assert.equal(timers.size, 1, "only one background wake should be scheduled");
    const timer = timers.values().next().value;
    timers.clear();
    state.now = timer.at;
    timer.callback();
  }
  async function finish(value, index = requests.length - 1) { requests[index].resolve(value); await flush(); }
  async function fail(index = requests.length - 1) { requests[index].reject(new Error("configuration is being replaced")); await flush(); }
  return { ...monitor, requests, applied, problems, timers, state, tick, finish, fail };
}

test("healthy, missing, and unreadable reports update settings without a background warning", async () => {
  const h = harness();
  h.wake();
  for (const status of ["healthy", "healthy", "missing", "unavailable"]) {
    await h.finish(report(status));
    assert.deepEqual(h.problems, []);
    assert.equal(h.timers.values().next().value.delay, 20_000);
    h.tick();
  }
  assert.deepEqual(h.applied.map((value) => value.status), ["healthy", "healthy", "missing", "unavailable"]);
  h.stop();
  await h.finish(report());
  assert.equal(h.timers.size, 0);
});

test("recovery needs a stable healthy file; a temporary deletion or empty rewrite cannot reset notification history", async () => {
  const recovered = [];
  const h = harness({ onRecovered: (value) => recovered.push(value) });
  h.wake();
  await h.finish(report());
  h.tick(); await h.finish(report("missing"));
  h.tick(); await h.finish(report("healthy", { fingerprint: "temporary-empty-file" }));
  h.tick(); await h.finish(report());
  assert.equal(recovered.length, 0);
  h.tick(); await h.finish(report("healthy"));
  h.state.now += 300; h.wake(); await h.finish(report("healthy"));
  assert.equal(recovered.length, 0);
  h.tick(); await h.finish(report("healthy"));
  assert.deepEqual(recovered.map((value) => value.fingerprint), ["healthy-file"]);
  h.stop();
});

test("temporary editor errors stay silent and a stable problem needs two separated reads", async () => {
  const h = harness();
  h.wake();
  await h.finish(report());
  assert.equal(h.problems.length, 0);
  assert.equal(h.timers.values().next().value.delay, 1_000);
  h.tick();
  await h.finish(report("healthy"));
  assert.equal(h.problems.length, 0);
  h.tick();
  await h.finish(report());
  assert.equal(h.problems.length, 0, "an earlier repaired problem must not count as confirmation");
  h.tick();
  await h.finish(report());
  assert.equal(h.problems.length, 1);
  h.stop();
});

test("a changing file restarts confirmation and rapid focus events do not bypass settling", async () => {
  const h = harness();
  const first = report("issues", { fingerprint: "partial-write-a" });
  const final = report("issues", { fingerprint: "final-write-b" });
  h.wake();
  await h.finish(first);
  h.state.now = 400;
  h.wake();
  await h.finish(final);
  h.state.now = 600;
  h.wake();
  await h.finish(final);
  assert.equal(h.problems.length, 0);
  assert.equal(h.timers.values().next().value.delay, 800);
  h.tick();
  await h.finish(final);
  assert.deepEqual(h.problems, [final]);
  assert.equal(h.state.now, 1_400);
  h.stop();
});

test("focus and visibility wakeups coalesce into one request and discard superseded results", async () => {
  const h = harness();
  h.wake(); h.wake(); h.wake();
  assert.equal(h.requests.length, 1);
  await h.finish(report());
  assert.equal(h.applied.length, 0);
  assert.equal(h.problems.length, 0);
  assert.equal(h.requests.length, 2, "pending wakeups must produce only one new request");
  await h.finish(report("healthy"));
  assert.deepEqual(h.applied.map((value) => value.status), ["healthy"]);
  assert.equal(h.timers.size, 1);
  h.stop();
});

test("provider edits and directory revisions discard stale results and restart confirmation", async () => {
  const h = harness();
  h.wake();
  h.state.revision += 1;
  await h.finish(report());
  assert.equal(h.applied.length, 0);
  h.tick();
  await h.finish(report());
  assert.equal(h.problems.length, 0);
  h.state.revision += 1;
  h.tick();
  await h.finish(report());
  assert.equal(h.problems.length, 0, "a candidate from the previous revision is not confirmation");
  h.tick();
  await h.finish(report());
  assert.equal(h.problems.length, 1);
  h.stop();
});

test("hidden windows cancel scheduled reads and ignore a read that finishes while hidden", async () => {
  const h = harness();
  h.wake();
  await h.finish(report("healthy"));
  h.state.visible = false;
  h.wake();
  assert.equal(h.requests.length, 1);
  assert.equal(h.timers.size, 0);
  h.state.visible = true;
  h.wake();
  assert.equal(h.requests.length, 2);
  h.state.visible = false;
  await h.finish(report());
  assert.equal(h.applied.length, 1);
  assert.equal(h.problems.length, 0);
  assert.equal(h.timers.size, 0);
  h.state.visible = true;
  h.wake();
  await h.finish(report("healthy"));
  assert.equal(h.applied.length, 2);
  h.stop();
});

test("configuration mutations block both starting and accepting background reads", async () => {
  const h = harness();
  h.state.readable = false;
  h.wake();
  assert.equal(h.requests.length, 0);
  h.state.readable = true;
  h.tick();
  h.state.readable = false;
  await h.finish(report());
  assert.equal(h.applied.length, 0);
  assert.equal(h.problems.length, 0);
  h.state.readable = true;
  h.tick();
  await h.finish(report());
  assert.equal(h.problems.length, 0);
  h.tick();
  await h.finish(report());
  assert.equal(h.problems.length, 1);
  h.stop();
});

test("an active dialog can defer the toast without stopping background reports", async () => {
  const h = harness();
  h.state.notifiable = false;
  h.wake();
  await h.finish(report());
  h.tick();
  await h.finish(report());
  assert.equal(h.applied.length, 2);
  assert.equal(h.problems.length, 0);
  h.state.notifiable = true;
  h.wake();
  await h.finish(report());
  assert.equal(h.problems.length, 1);
  h.stop();
});

test("transient IPC failures stay silent and break the confirmation streak", async () => {
  const h = harness();
  h.wake();
  await h.finish(report());
  h.tick();
  await h.fail();
  assert.equal(h.problems.length, 0);
  assert.equal(h.applied.length, 1);
  h.tick();
  await h.finish(report());
  assert.equal(h.problems.length, 0);
  h.tick();
  await h.finish(report());
  assert.equal(h.problems.length, 1);
  h.stop();
});

test("stopping during a pending wake prevents all late reports and future timers", async () => {
  const h = harness();
  h.wake(); h.wake();
  h.stop();
  await h.finish(report());
  h.wake();
  assert.equal(h.requests.length, 1);
  assert.equal(h.applied.length, 0);
  assert.equal(h.problems.length, 0);
  assert.equal(h.timers.size, 0);
});

function memoryStorage(initial) {
  const values = new Map(initial ? [["codexx.configHealth.notified.v1", initial]] : []);
  return { values, getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
}

test("notice suppression survives restarts and ignores unrelated TOML edits and issue ordering", () => {
  const storage = memoryStorage();
  const original = report("issues", { issues: [issue, { ...issue, code: "invalid-wire-api", title: "接口配置错误" }] });
  const first = createConfigHealthNoticeRegistry(storage);
  assert.equal(first.has(original), false);
  first.mark(original);
  const reopened = createConfigHealthNoticeRegistry(storage);
  assert.equal(reopened.has(original), true);
  assert.equal(reopened.has({ ...original, fingerprint: "different-comment", checkedAt: "2026-09-11T00:00:00Z", issues: [...original.issues].reverse() }), true);
  assert.equal(reopened.has({ ...original, issues: [issue] }), false);
  const persisted = [...storage.values.values()].join("");
  assert.equal(persisted.includes(original.codexDir), false, "notification storage does not need raw configuration paths");
  assert.equal(persisted.includes(issue.description), false, "notification storage does not need issue details");
});

test("suppression is scoped to the configuration directory and recovery permits a later warning", async () => {
  const storage = memoryStorage();
  const seen = createConfigHealthNoticeRegistry(storage);
  const notices = [];
  const h = harness({
    onReport: (value) => { if (value.status === "healthy") seen.clear(value); },
    onProblem: (value) => { if (!seen.has(value)) { seen.mark(value); notices.push(value); } },
  });
  h.wake();
  await h.finish(report());
  h.tick();
  await h.finish(report());
  h.tick();
  await h.finish(report());
  assert.equal(notices.length, 1);
  assert.equal(seen.has(report("issues", { codexDir: "/fixtures/other-codex" })), false);
  h.tick();
  await h.finish(report("healthy"));
  assert.equal(createConfigHealthNoticeRegistry(storage).has(report()), false);
  h.tick();
  await h.finish(report());
  assert.equal(notices.length, 1, "a recovered issue must still be confirmed again");
  h.tick();
  await h.finish(report());
  assert.equal(notices.length, 2);
  h.stop();
});

test("registry recovery only clears its own directory and normalizes Windows path separators", () => {
  const seen = createConfigHealthNoticeRegistry(memoryStorage());
  const windows = report("issues", { codexDir: "C:\\Users\\Sample\\.codex" });
  const other = report("issues", { codexDir: "/fixtures/other-codex" });
  seen.mark(windows);
  seen.mark(other);
  assert.equal(seen.has({ ...windows, codexDir: "C:/Users/Sample/.codex" }), true);
  seen.clear({ ...windows, status: "healthy", issues: [] });
  assert.equal(seen.has(windows), false);
  assert.equal(seen.has(other), true);
});

test("blocked or malformed browser storage falls back to in-memory notification suppression", () => {
  for (const storage of [
    undefined,
    { getItem() { throw new Error("storage blocked"); }, setItem() { throw new Error("storage blocked"); } },
    memoryStorage("invalid JSON"),
    memoryStorage(JSON.stringify({ scope: "not-an-array" })),
    memoryStorage(JSON.stringify([null, {}, "bad-entry", { scope: 1, issue: [] }])),
  ]) {
    const seen = createConfigHealthNoticeRegistry(storage);
    assert.equal(seen.has(report()), false);
    seen.mark(report());
    assert.equal(seen.has(report()), true);
    seen.clear(report("healthy"));
    assert.equal(seen.has(report()), false);
  }
});
