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

/** Sum two decimal credit strings exactly, so a total never rounds up past the parts. */
export function addCredits(a: string, b: string): string {
  const scaled = (value: string) => {
    const match = /^(\d+)(?:\.(\d+))?$/.exec(value);
    if (!match) return 0n;
    return BigInt(match[1]) * 1_000_000n + BigInt((match[2] ?? "").slice(0, 6).padEnd(6, "0"));
  };
  const total = scaled(a) + scaled(b);
  const fraction = (total % 1_000_000n).toString().padStart(6, "0").replace(/0+$/, "");
  return (total / 1_000_000n).toString() + (fraction ? "." + fraction : "");
}

/** A glanceable balance: whole credits once there are enough for fractions not to matter. */
export function creditsShort(value: string): string {
  const whole = /^(\d+)/.exec(value)?.[1];
  return whole && BigInt(whole) >= 100n ? BigInt(whole).toLocaleString() : credits(value);
}

/** Plan identifiers arrive lowercase ("pro"); "no plan" variants mean none. */
export function planName(plan: string | undefined): string | undefined {
  const value = plan?.trim();
  if (!value || /^(no[ _-]?plan|none|free)$/i.test(value)) return undefined;
  return value.charAt(0).toUpperCase() + value.slice(1);
}
