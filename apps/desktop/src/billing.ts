/** Public account data only. Cloud credentials stay in the native account client. */
export type BillingState = {
  loggedIn: boolean;
  error?: string | null;
  summary?: {
    accountId: string;
    accountName: string;
    plan: string;
    subscriptionStatus?: string | null;
    accountStatus: string;
    includedCredits: string;
    purchasedCredits: string;
    heldCredits: string;
    cloudUnits: string;
    cloudUnitsHeld: string;
    resetsAt?: string | null;
    periodEndsAt?: string | null;
    cancelAtPeriodEnd: boolean;
    overageOptIn: boolean;
    billingUrl: string;
    usageUrl: string;
  } | null;
};

/** Round down at the display boundary so tiny balances never become overspendable. */
export function credits(value: string): string {
  const match = /^(\d+)(?:\.(\d+))?$/.exec(value);
  if (!match) return "—";
  const whole = BigInt(match[1]).toLocaleString();
  const fraction = (match[2] ?? "").slice(0, 2).replace(/0+$/, "");
  if (whole === "0" && !fraction && /[1-9]/.test(match[2] ?? "")) return "<0.01";
  return fraction ? `${whole}.${fraction}` : whole;
}
