const DAY_MS = 24 * 60 * 60 * 1000;
const MAX_RENDER_POINTS = 420;

function finiteNumber(value, fallback = 0) {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function timestamp(value, fallback = Date.now()) {
  const parsed = typeof value === "number" ? value : Date.parse(value);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function appendPoint(points, point) {
  const next = {
    timestamp: Math.max(0, Math.round(finiteNumber(point.timestamp))),
    count: Math.max(0, Math.round(finiteNumber(point.count))),
  };
  const last = points.at(-1);
  if (last && last.timestamp === next.timestamp) {
    last.count = next.count;
    return;
  }
  points.push(next);
}

export function escapeXml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

export function buildBaseline({ repository, createdAt, currentStars, stargazers, checkedAt }) {
  const checkedTimestamp = timestamp(checkedAt);
  const uniqueUsers = new Map();

  for (const item of Array.isArray(stargazers) ? stargazers : []) {
    const userId = item?.user?.id ?? item?.user?.login;
    const starredAt = Date.parse(item?.starred_at || "");
    if (userId == null || !Number.isFinite(starredAt)) continue;
    const key = String(userId);
    const previous = uniqueUsers.get(key);
    if (previous == null || starredAt < previous) uniqueUsers.set(key, starredAt);
  }

  const starTimes = [...uniqueUsers.values()].sort((a, b) => a - b);
  const createdTimestamp = timestamp(createdAt, starTimes[0] ?? checkedTimestamp);
  const startTimestamp = Math.min(createdTimestamp, starTimes[0] ?? createdTimestamp);
  const points = [];
  appendPoint(points, { timestamp: startTimestamp, count: 0 });

  let count = 0;
  let currentDay = "";
  for (const starredAt of starTimes) {
    count += 1;
    const day = new Date(starredAt).toISOString().slice(0, 10);
    if (day === currentDay) {
      points.at(-1).timestamp = starredAt;
      points.at(-1).count = count;
    } else {
      currentDay = day;
      appendPoint(points, { timestamp: starredAt, count });
    }
  }

  const reportedStars = Math.max(0, Math.round(finiteNumber(currentStars)));
  appendPoint(points, {
    timestamp: Math.max(checkedTimestamp, points.at(-1)?.timestamp ?? checkedTimestamp),
    count: reportedStars,
  });

  return {
    schemaVersion: 1,
    repository,
    createdAt: new Date(createdTimestamp).toISOString(),
    initializedAt: new Date(checkedTimestamp).toISOString(),
    checkedAt: new Date(checkedTimestamp).toISOString(),
    currentStars: reportedStars,
    baseline: points,
    snapshots: [],
    source: {
      uniqueStargazers: uniqueUsers.size,
      reportedStars,
      consistencyDelta: reportedStars - uniqueUsers.size,
    },
  };
}

export function mergeObservation(existing, { currentStars, checkedAt }) {
  if (!existing?.baseline?.length) throw new Error("Star history baseline is missing");
  const observedAt = timestamp(checkedAt);
  const existingCheckedAt = Date.parse(existing.checkedAt || "") || 0;
  if (observedAt <= existingCheckedAt) return existing;
  const reportedStars = Math.max(0, Math.round(finiteNumber(currentStars)));
  const snapshots = Array.isArray(existing.snapshots) ? [...existing.snapshots] : [];
  const allPoints = [...existing.baseline, ...snapshots];
  const last = allPoints.at(-1);
  const nextTimestamp = Math.max(observedAt, (last?.timestamp ?? observedAt) + 1);
  const shouldRecord = !last || last.count !== reportedStars || nextTimestamp - last.timestamp >= DAY_MS;
  if (shouldRecord) appendPoint(snapshots, { timestamp: nextTimestamp, count: reportedStars });

  return {
    ...existing,
    checkedAt: new Date(observedAt).toISOString(),
    currentStars: reportedStars,
    snapshots,
  };
}

export function chartPoints(dataset) {
  const combined = [...(dataset?.baseline || []), ...(dataset?.snapshots || [])]
    .filter((point) => Number.isFinite(point?.timestamp) && Number.isFinite(point?.count))
    .map((point) => ({
      timestamp: Math.round(point.timestamp),
      count: Math.max(0, Math.round(point.count)),
    }))
    .sort((a, b) => a.timestamp - b.timestamp);

  const normalized = [];
  for (const point of combined) appendPoint(normalized, point);
  return normalized;
}

export function downsamplePoints(points, limit = MAX_RENDER_POINTS) {
  if (points.length <= limit) return points;
  const sampled = [];
  for (let index = 0; index < limit; index += 1) {
    const sourceIndex = Math.round((index * (points.length - 1)) / (limit - 1));
    const point = points[sourceIndex];
    if (sampled.at(-1) !== point) sampled.push(point);
  }
  return sampled;
}

function niceStep(maxValue, targetTicks = 5) {
  const raw = Math.max(1, maxValue) / targetTicks;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const residual = raw / magnitude;
  const nice = residual <= 1 ? 1 : residual <= 2 ? 2 : residual <= 5 ? 5 : 10;
  return nice * magnitude;
}

function formatDate(value, span) {
  const date = new Date(value);
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  if (span < 2 * DAY_MS) {
    const hour = String(date.getUTCHours()).padStart(2, "0");
    const minute = String(date.getUTCMinutes()).padStart(2, "0");
    return `${months[date.getUTCMonth()]} ${date.getUTCDate()} ${hour}:${minute}`;
  }
  if (span < 370 * DAY_MS) return `${months[date.getUTCMonth()]} ${date.getUTCDate()}`;
  return `${months[date.getUTCMonth()]} ${date.getUTCFullYear()}`;
}

function formatUpdated(value) {
  const date = new Date(value);
  const year = date.getUTCFullYear();
  const month = String(date.getUTCMonth() + 1).padStart(2, "0");
  const day = String(date.getUTCDate()).padStart(2, "0");
  const hour = String(date.getUTCHours()).padStart(2, "0");
  const minute = String(date.getUTCMinutes()).padStart(2, "0");
  return `${year}-${month}-${day} ${hour}:${minute} UTC`;
}

function formatInteger(value) {
  return String(Math.max(0, Math.round(finiteNumber(value)))).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

function svgTheme(theme) {
  if (theme === "dark") {
    return {
      background: "#0d1117",
      border: "#30363d",
      grid: "#21262d",
      text: "#f0f6fc",
      muted: "#8b949e",
      line: "#58a6ff",
      fill: "#173654",
      badge: "#161b22",
      badgeBorder: "#30363d",
    };
  }
  return {
    background: "#ffffff",
    border: "#d0d7de",
    grid: "#eaeef2",
    text: "#1f2328",
    muted: "#656d76",
    line: "#0969da",
    fill: "#dbeafe",
    badge: "#f6f8fa",
    badgeBorder: "#d0d7de",
  };
}

export function renderStarHistorySvg(dataset, theme = "light") {
  const colors = svgTheme(theme);
  const width = 900;
  const height = 500;
  const left = 76;
  const right = 34;
  const top = 126;
  const bottom = 72;
  const plotWidth = width - left - right;
  const plotHeight = height - top - bottom;
  const rawPoints = chartPoints(dataset);
  const points = downsamplePoints(rawPoints);
  const fallbackTimestamp = timestamp(dataset?.checkedAt);
  const xMin = points[0]?.timestamp ?? fallbackTimestamp - DAY_MS;
  let xMax = Math.max(points.at(-1)?.timestamp ?? fallbackTimestamp, fallbackTimestamp);
  if (xMax <= xMin) xMax = xMin + DAY_MS;
  const maxCount = Math.max(dataset?.currentStars || 0, ...points.map((point) => point.count), 1);
  const yStep = niceStep(maxCount);
  const yMax = Math.max(yStep, Math.ceil(maxCount / yStep) * yStep);
  const xSpan = xMax - xMin;
  const scaleX = (value) => left + ((value - xMin) / xSpan) * plotWidth;
  const scaleY = (value) => top + plotHeight - (value / yMax) * plotHeight;
  const plotPoints = points.map((point) => `${scaleX(point.timestamp).toFixed(1)},${scaleY(point.count).toFixed(1)}`);
  const linePath = plotPoints.length ? `M${plotPoints.join(" L")}` : "";
  const areaPath = plotPoints.length
    ? `${linePath} L${scaleX(points.at(-1).timestamp).toFixed(1)},${(top + plotHeight).toFixed(1)} L${scaleX(points[0].timestamp).toFixed(1)},${(top + plotHeight).toFixed(1)} Z`
    : "";

  const yGrid = [];
  for (let value = 0; value <= yMax + yStep / 2; value += yStep) {
    const y = scaleY(value);
    yGrid.push(`<line x1="${left}" y1="${y.toFixed(1)}" x2="${left + plotWidth}" y2="${y.toFixed(1)}" class="grid"/>`);
    yGrid.push(`<text x="${left - 14}" y="${(y + 5).toFixed(1)}" class="axis-label" text-anchor="end">${Math.round(value)}</text>`);
  }

  const xGrid = [];
  for (let index = 0; index < 5; index += 1) {
    const ratio = index / 4;
    const value = xMin + xSpan * ratio;
    const x = left + plotWidth * ratio;
    xGrid.push(`<line x1="${x.toFixed(1)}" y1="${top}" x2="${x.toFixed(1)}" y2="${top + plotHeight}" class="grid vertical"/>`);
    xGrid.push(`<text x="${x.toFixed(1)}" y="${top + plotHeight + 28}" class="axis-label" text-anchor="middle">${escapeXml(formatDate(value, xSpan))}</text>`);
  }

  const repository = escapeXml(dataset?.repository || "Unknown repository");
  const currentStars = Math.max(0, Math.round(finiteNumber(dataset?.currentStars)));
  const formattedStars = formatInteger(currentStars);
  const updated = escapeXml(formatUpdated(dataset?.checkedAt || fallbackTimestamp));
  const lastPoint = points.at(-1);
  const lastMarker = lastPoint
    ? `<circle cx="${scaleX(lastPoint.timestamp).toFixed(1)}" cy="${scaleY(lastPoint.count).toFixed(1)}" r="5" class="last-point"/>`
    : "";

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" role="img" aria-labelledby="title description">
  <title id="title">Star History for ${repository}</title>
  <desc id="description">${formattedStars} current GitHub stars, updated ${updated}</desc>
  <style>
    text { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; }
    .title { fill: ${colors.text}; font-size: 24px; font-weight: 700; }
    .repo { fill: ${colors.muted}; font-size: 14px; }
    .axis-label { fill: ${colors.muted}; font-size: 12px; }
    .legend { fill: ${colors.text}; font-size: 13px; font-weight: 600; }
    .count { fill: ${colors.text}; font-size: 18px; font-weight: 700; }
    .count-label { fill: ${colors.muted}; font-size: 11px; }
    .footer { fill: ${colors.muted}; font-size: 11px; }
    .grid { stroke: ${colors.grid}; stroke-width: 1; }
    .grid.vertical { stroke-dasharray: 3 5; }
    .series { fill: none; stroke: ${colors.line}; stroke-width: 3; stroke-linecap: round; stroke-linejoin: round; }
    .area { fill: ${colors.fill}; opacity: .48; }
    .last-point { fill: ${colors.background}; stroke: ${colors.line}; stroke-width: 3; }
  </style>
  <rect width="${width}" height="${height}" rx="8" fill="${colors.background}" stroke="${colors.border}"/>
  <text x="36" y="42" class="title">Star History</text>
  <text x="36" y="68" class="repo">${repository}</text>
  <text x="36" y="91" class="repo">Updated ${updated}</text>
  <g transform="translate(716 28)">
    <rect width="148" height="60" rx="7" fill="${colors.badge}" stroke="${colors.badgeBorder}"/>
    <text x="18" y="24" class="count-label">CURRENT STARS</text>
    <text x="18" y="48" class="count">&#9733; ${formattedStars}</text>
  </g>
  ${yGrid.join("\n  ")}
  ${xGrid.join("\n  ")}
  ${areaPath ? `<path d="${areaPath}" class="area"/>` : ""}
  ${linePath ? `<path d="${linePath}" class="series"/>` : ""}
  ${lastMarker}
  <g transform="translate(${left + 16} ${top + 18})">
    <line x1="0" y1="0" x2="24" y2="0" class="series"/>
    <text x="34" y="5" class="legend">${repository}</text>
  </g>
  <text x="${width / 2}" y="${height - 20}" class="footer" text-anchor="middle">Live snapshots via GitHub Actions · Served by Cloudflare CDN</text>
</svg>`;
}

export function renderUnavailableSvg(repository, theme = "light") {
  const colors = svgTheme(theme);
  const label = escapeXml(repository || "Star History");
  return `<svg xmlns="http://www.w3.org/2000/svg" width="900" height="500" viewBox="0 0 900 500" role="img" aria-label="Star History is waiting for its first sync">
  <rect width="900" height="500" rx="8" fill="${colors.background}" stroke="${colors.border}"/>
  <text x="450" y="210" text-anchor="middle" fill="${colors.text}" font-family="-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif" font-size="25" font-weight="700">Star History</text>
  <text x="450" y="250" text-anchor="middle" fill="${colors.muted}" font-family="-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif" font-size="15">${label}</text>
  <text x="450" y="284" text-anchor="middle" fill="${colors.muted}" font-family="-apple-system, BlinkMacSystemFont, Segoe UI, sans-serif" font-size="14">Waiting for the first authenticated GitHub sync...</text>
</svg>`;
}
