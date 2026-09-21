import { CheckCircle2, Crown, Gem, KeyRound, Mail, ShieldCheck, Sparkles, UserRound } from "lucide-react";
import { getOfficialPlan } from "../officialPlan";
import "../styles/official-account-badge.css";

export type OfficialAccountBadgeProps = {
  lang: "zh" | "en";
  email?: string | null;
  hasAuth: boolean;
  canQueryQuota: boolean;
  planType?: string | null;
  className?: string;
};

export function OfficialAccountBadge({ lang, email, hasAuth, canQueryQuota, planType, className = "" }: OfficialAccountBadgeProps) {
  const signedOut = !hasAuth;
  const label = signedOut
    ? (lang === "zh" ? "未登录" : "Not signed in")
    : !canQueryQuota
      ? (lang === "zh" ? "已保存认证" : "Saved credentials")
      : email || (lang === "zh" ? "已登录" : "Signed in");
  const Icon = signedOut ? UserRound : !canQueryQuota ? KeyRound : email ? Mail : CheckCircle2;
  const state = signedOut ? "signed-out" : canQueryQuota ? "signed-in" : "saved";
  const plan = hasAuth && canQueryQuota ? getOfficialPlan(planType, lang) : null;
  const PlanIcon = plan?.tone === "pro20" ? Crown : plan?.tone === "pro5" ? Gem : plan?.tone === "enterprise" ? ShieldCheck : Sparkles;

  return <span className={`cx-official-account ${className}`.trim()}>
    <span className={`cx-official-account-badge cx-official-account-badge--${state}`} title={label}>
      <Icon size={14} strokeWidth={1.8} aria-hidden="true" />
      <span>{label}</span>
    </span>
    {plan && <span className={`cx-official-plan cx-official-plan--${plan.tone}`} title={`${lang === "zh" ? "套餐" : "Plan"}：${plan.label}`}>
      {plan.tone !== "free" && plan.tone !== "neutral" && <PlanIcon size={12} aria-hidden="true" />}
      <span>{plan.label}</span>
    </span>}
  </span>;
}
