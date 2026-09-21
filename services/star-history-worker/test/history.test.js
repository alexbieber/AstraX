import assert from "node:assert/strict";
import test from "node:test";

import {
  buildBaseline,
  chartPoints,
  downsamplePoints,
  escapeXml,
  mergeObservation,
  renderStarHistorySvg,
  renderUnavailableSvg,
} from "../src/history.js";

test("buildBaseline deduplicates users and preserves the reported current count", () => {
  const dataset = buildBaseline({
    repository: "owner/repo",
    createdAt: "2026-01-01T00:00:00Z",
    currentStars: 2,
    checkedAt: "2026-01-04T00:00:00Z",
    stargazers: [
      { starred_at: "2026-01-02T00:00:00Z", user: { id: 1 } },
      { starred_at: "2026-01-03T00:00:00Z", user: { id: 2 } },
      { starred_at: "2026-01-03T01:00:00Z", user: { id: 2 } },
    ],
  });

  assert.equal(dataset.source.uniqueStargazers, 2);
  assert.equal(dataset.currentStars, 2);
  assert.equal(chartPoints(dataset).at(-1).count, 2);
});

test("mergeObservation records changes and avoids noisy unchanged snapshots", () => {
  const baseline = buildBaseline({
    repository: "owner/repo",
    createdAt: "2026-01-01T00:00:00Z",
    currentStars: 1,
    checkedAt: "2026-01-02T00:00:00Z",
    stargazers: [{ starred_at: "2026-01-02T00:00:00Z", user: { id: 1 } }],
  });
  const unchanged = mergeObservation(baseline, {
    currentStars: 1,
    checkedAt: "2026-01-02T00:15:00Z",
  });
  const changed = mergeObservation(unchanged, {
    currentStars: 2,
    checkedAt: "2026-01-02T00:30:00Z",
  });

  assert.equal(unchanged.snapshots.length, 0);
  assert.equal(changed.snapshots.length, 1);
  assert.equal(changed.snapshots[0].count, 2);
});

test("mergeObservation ignores out-of-order observations", () => {
  const baseline = buildBaseline({
    repository: "owner/repo",
    createdAt: "2026-01-01T00:00:00Z",
    currentStars: 10,
    checkedAt: "2026-01-02T00:02:00Z",
    stargazers: [],
  });
  const result = mergeObservation(baseline, {
    currentStars: 9,
    checkedAt: "2026-01-02T00:01:00Z",
  });

  assert.equal(result.currentStars, 10);
  assert.equal(result.checkedAt, baseline.checkedAt);
  assert.equal(result.snapshots.length, 0);
});

test("buildBaseline aggregates large histories by UTC day", () => {
  const stargazers = Array.from({ length: 500 }, (_, index) => ({
    starred_at: `2026-01-${String((index % 5) + 1).padStart(2, "0")}T${String(index % 24).padStart(2, "0")}:00:00Z`,
    user: { id: index + 1 },
  }));
  const dataset = buildBaseline({
    repository: "owner/repo",
    createdAt: "2026-01-01T00:00:00Z",
    currentStars: 500,
    checkedAt: "2026-01-06T00:00:00Z",
    stargazers,
  });

  assert.ok(dataset.baseline.length <= 7);
  assert.equal(dataset.baseline.at(-1).count, 500);
});

test("downsamplePoints keeps the first and last point", () => {
  const points = Array.from({ length: 1000 }, (_, index) => ({ timestamp: index, count: index }));
  const sampled = downsamplePoints(points, 100);
  assert.equal(sampled.length, 100);
  assert.deepEqual(sampled[0], points[0]);
  assert.deepEqual(sampled.at(-1), points.at(-1));
});

test("SVG rendering supports light, dark, zero-star, and XML escaping", () => {
  const dataset = buildBaseline({
    repository: 'owner/<repo>&"',
    createdAt: "2026-01-01T00:00:00Z",
    currentStars: 0,
    checkedAt: "2026-01-02T00:00:00Z",
    stargazers: [],
  });
  const light = renderStarHistorySvg(dataset, "light");
  const dark = renderStarHistorySvg(dataset, "dark");
  const unavailable = renderUnavailableSvg("owner/repo", "dark");

  assert.match(light, /^<svg/);
  assert.match(light, /owner\/&lt;repo&gt;&amp;&quot;/);
  assert.doesNotMatch(light, /NaN|undefined/);
  assert.match(dark, /#0d1117/);
  assert.match(unavailable, /Waiting for the first authenticated GitHub sync/);
  assert.equal(escapeXml("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
});
