import assert from "node:assert/strict";
import test from "node:test";
import { createOfficialProfileMonitor } from "../src/officialProfileMonitor.ts";

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const flush = async () => { await Promise.resolve(); await Promise.resolve(); };
const signedIn = [{ id: "official", email: "login@example.test", hasOwnedAuth: true, canQueryQuota: true }];
const signedOut = [{ id: "official", email: null, hasOwnedAuth: false, canQueryQuota: false }];

function harness() {
  const requests = [], applied = [], timers = new Map();
  let nextTimer = 0;
  const state = { visible: true, ready: true, revision: 0 };
  const monitor = createOfficialProfileMonitor({
    read: () => { const request = deferred(); requests.push(request); return request.promise; },
    apply: (value) => applied.push(value),
    revision: () => state.revision,
    isVisible: () => state.visible,
    canRead: () => state.ready,
    schedule: (callback, delay) => {
      assert.equal(delay, 2_000);
      const id = ++nextTimer;
      timers.set(id, callback);
      return () => timers.delete(id);
    },
  });
  function tick() {
    assert.equal(timers.size, 1);
    const callback = timers.values().next().value;
    timers.clear();
    callback();
  }
  return { ...monitor, requests, applied, timers, state, tick };
}

test("external login and logout are reflected by scheduled metadata reads", async () => {
  const h = harness();
  h.wake();
  h.requests[0].resolve(signedOut);
  await flush();
  h.tick();
  h.requests[1].resolve(signedIn);
  await flush();
  h.tick();
  h.requests[2].resolve(signedOut);
  await flush();
  assert.deepEqual(h.applied, [signedOut, signedIn, signedOut]);
  h.stop();
  assert.equal(h.timers.size, 0);
});

test("focus and visibility events coalesce without concurrent reads", async () => {
  const h = harness();
  h.wake(); h.wake(); h.wake();
  assert.equal(h.requests.length, 1);
  h.requests[0].resolve(signedOut);
  await flush();
  assert.deepEqual(h.applied, []);
  assert.equal(h.requests.length, 2);
  h.requests[1].resolve(signedIn);
  await flush();
  assert.deepEqual(h.applied, [signedIn]);
  assert.equal(h.timers.size, 1);
  h.stop();
});

test("an earlier poll cannot overwrite a newer save, deletion, or directory revision", async () => {
  const h = harness();
  h.wake();
  h.state.revision += 1;
  h.requests[0].resolve(signedOut);
  await flush();
  assert.deepEqual(h.applied, []);
  h.tick();
  h.requests[1].resolve(signedIn);
  await flush();
  assert.deepEqual(h.applied, [signedIn]);
  h.stop();
});

test("mutations block both sending and accepting background metadata", async () => {
  const h = harness();
  h.state.ready = false;
  h.wake();
  assert.equal(h.requests.length, 0);
  h.state.ready = true;
  h.tick();
  h.state.ready = false;
  h.requests[0].resolve(signedOut);
  await flush();
  assert.deepEqual(h.applied, []);
  h.state.ready = true;
  h.tick();
  h.requests[1].resolve(signedIn);
  await flush();
  assert.deepEqual(h.applied, [signedIn]);
  h.stop();
});

test("hidden windows pause reads and resume immediately on wake", async () => {
  const h = harness();
  h.wake();
  h.requests[0].resolve(signedOut);
  await flush();
  h.state.visible = false;
  h.wake();
  assert.equal(h.requests.length, 1);
  assert.equal(h.timers.size, 0);
  h.state.visible = true;
  h.wake();
  assert.equal(h.requests.length, 2);
  h.stop();
  h.requests[1].resolve(signedIn);
  await flush();
  assert.deepEqual(h.applied, [signedOut]);
  assert.equal(h.timers.size, 0);
});

test("temporary read failures retry silently and stopped readers never reschedule", async () => {
  const h = harness();
  h.wake();
  h.requests[0].reject(new Error("auth file is being replaced"));
  await flush();
  assert.deepEqual(h.applied, []);
  h.tick();
  h.stop();
  h.requests[1].resolve(signedIn);
  await flush();
  h.wake();
  assert.equal(h.requests.length, 2);
  assert.equal(h.timers.size, 0);
  assert.deepEqual(h.applied, []);
});
