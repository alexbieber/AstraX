export type OfficialPlanTone = "pro20" | "pro5" | "plus" | "team" | "enterprise" | "edu" | "free" | "neutral";
export type OfficialPlan = { label: string; tone: OfficialPlanTone };

// Display aliases requested for the account UI; unknown plans remain neutral.
export function getOfficialPlan(planType: string | null | undefined, lang: "zh" | "en"): OfficialPlan | null {
  if (typeof planType !== "string" || planType.length > 64 || /[\u0000-\u001f\u007f-\u009f]/.test(planType)) return null;
  const raw = planType.trim();
  if (!raw) return null;
  const key = raw.toLowerCase().replace(/[_\s]+/g, "-");
  if (["pro", "pro20x", "pro-20x"].includes(key)) return { label: "Pro 20x", tone: "pro20" };
  if (["prolite", "pro-lite", "pro5x", "pro-5x"].includes(key)) return { label: "Pro 5x", tone: "pro5" };
  const known: Record<string, OfficialPlan> = {
    plus: { label: "Plus", tone: "plus" },
    team: { label: "Team", tone: "team" },
    business: { label: "Business", tone: "team" },
    enterprise: { label: "Enterprise", tone: "enterprise" },
    edu: { label: "Edu", tone: "edu" },
    free: { label: lang === "zh" ? "免费版" : "Free", tone: "free" },
    go: { label: "Go", tone: "neutral" },
  };
  return Object.prototype.hasOwnProperty.call(known, key) ? known[key] : { label: raw, tone: "neutral" };
}
