import assert from "node:assert/strict";
import test from "node:test";
import { parseRoutingDraft, routingDraft, routingFields, sameRoutingDraft } from "../src/routingSettings.ts";

const saved = () => ({ version: 2, routerEnabled: false, takeoverEnabled: false, autoFailoverEnabled: false, listenAddress: "127.0.0.1", listenPort: 15721,
  providerIds: [], maxRetries: 3, streamingFirstByteTimeout: 60, streamingIdleTimeout: 120, nonStreamingTimeout: 600,
  circuitFailureThreshold: 4, circuitSuccessThreshold: 2, circuitTimeoutSeconds: 60, circuitErrorRateThreshold: 0.6, circuitMinRequests: 10 });

test("saved routing settings roundtrip without changing switches, queue or percentage", () => {
  const settings = { ...saved(), routerEnabled: true, takeoverEnabled: true, autoFailoverEnabled: true, providerIds: ["p2", "p1"], circuitErrorRateThreshold: 0.625 };
  const draft = routingDraft(settings);
  assert.equal(draft.circuitErrorRateThreshold, "62.5");
  assert.deepEqual(parseRoutingDraft(draft), { settings, errors: {} });
  draft.providerIds.reverse();
  assert.deepEqual(settings.providerIds, ["p2", "p1"]);
});

test("routing and saved failover preference remain independent when the service is stopped", () => {
  const settings = { ...saved(), autoFailoverEnabled: true, providerIds: ["only-one"], streamingIdleTimeout: 0 };
  assert.deepEqual(parseRoutingDraft(routingDraft(settings)).settings, settings);
  assert.ok(parseRoutingDraft(routingDraft({ ...settings, providerIds: [] })).settings);
});

test("numeric fields accept their documented boundaries and reject empty, fractional or out-of-range values", () => {
  for (const field of routingFields) {
    for (const boundary of [field.min, field.max]) {
      const result = parseRoutingDraft({ ...routingDraft(saved()), [field.key]: String(boundary) });
      assert.ok(result.settings, `${field.key}: ${boundary}`);
    }
    const invalid = ["", " ", "NaN", "1e2", String(field.min - 1), String(field.max + 1)];
    if (field.key !== "circuitErrorRateThreshold") invalid.push("1.5");
    for (const value of invalid) {
      const result = parseRoutingDraft({ ...routingDraft(saved()), [field.key]: value });
      assert.equal(result.settings, null, `${field.key}: ${value}`);
      assert.ok(result.errors[field.key]);
    }
  }
});

test("listen settings allow IPv4, IPv6 and localhost but never coerce URLs or invalid ports", () => {
  for (const address of ["127.0.0.1", "0.0.0.0", "::1", "::", "2001:db8::1", "localhost"]) {
    const result = parseRoutingDraft({ ...routingDraft(saved()), listenAddress: address });
    assert.ok(result.settings, address);
    assert.equal(result.settings.listenAddress, address === "localhost" ? "127.0.0.1" : address);
  }
  for (const address of ["", "999.0.0.1", "127.000.0.1", "https://localhost", "::1/path", "example.com", "[::1]"]) {
    assert.ok(parseRoutingDraft({ ...routingDraft(saved()), listenAddress: address }).errors.listenAddress, address);
  }
  for (const port of ["", "12x", "1023", "65536", "15721.5"]) {
    assert.ok(parseRoutingDraft({ ...routingDraft(saved()), listenPort: port }).errors.listenPort, port);
  }
  for (const port of ["1024", "65535"]) assert.ok(parseRoutingDraft({ ...routingDraft(saved()), listenPort: port }).settings);
});

test("the queue accepts one through 64 providers without an eight-provider limit and rejects duplicates", () => {
  const draft = routingDraft(saved());
  assert.ok(parseRoutingDraft({ ...draft, providerIds: ["single"] }).settings);
  assert.ok(parseRoutingDraft({ ...draft, providerIds: Array.from({ length: 64 }, (_, i) => String(i)) }).settings);
  assert.ok(parseRoutingDraft({ ...draft, providerIds: Array.from({ length: 65 }, (_, i) => String(i)) }).errors.providerIds);
  assert.ok(parseRoutingDraft({ ...draft, providerIds: ["same", "same"] }).errors.providerIds);
});

test("draft comparison detects independent switch, listening, tuning and queue changes", () => {
  const original = routingDraft({ ...saved(), providerIds: ["a", "b"] });
  assert.ok(sameRoutingDraft(original, routingDraft({ ...saved(), providerIds: ["a", "b"] })));
  for (const change of [{ routerEnabled: true }, { takeoverEnabled: true }, { autoFailoverEnabled: true }, { listenAddress: "::1" }, { listenPort: "15722" }, { providerIds: ["b", "a"] }, ...routingFields.map(({ key }) => ({ [key]: original[key] + "0" }))]) {
    assert.equal(sameRoutingDraft(original, { ...original, ...change }), false);
  }
});
