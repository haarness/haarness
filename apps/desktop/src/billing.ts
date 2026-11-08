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
    /** Credits left, counting what requests in flight hold, so the balance does not dip during a reply. */
    includedCredits: string;
    purchasedCredits: string;
    /** Credits requests in flight hold; part of the balances above. */
    heldCredits: string;
    cloudUnits: string;
    cloudUnitsHeld: string;
    resetsAt?: string | null;
    periodEndsAt?: string | null;
    cancelAtPeriodEnd: boolean;
    overageOptIn: boolean;
    billingUrl: string;
    usageUrl: string;
    /** Seats on a per-seat (Team) plan. */
    seats?: number | null;
    /** `month` or `year` for a subscription. */
    billingInterval?: string | null;
    /** The current allowance is the one-time trial. */
    trial?: boolean;
    trialAvailable?: boolean;
    /** A trial checkout saved a card that had already been used for a trial. */
    trialRefused?: boolean;
    /** Purchased credits are on hold and pay for nothing. */
    purchasedFrozen?: boolean;
    /** Purchased credits pay for model requests now: overage is on, or there is no plan or trial. Never while on
     * hold. Absent from an older Haarness, which let them pay only with overage on (see `purchasedPays`). */
    purchasedPays?: boolean;
    /** The purchased balance is a new account's free starter credits, with nothing bought yet. */
    purchasedIsStarter?: boolean;
    autoTopup?: boolean;
    autoTopupFailure?: string | null;
    /** Days the credits that can pay last at the last week's pace. */
    daysLeft?: number | null;
    /** The account has a plan or has held credits (purchased or starter ones), yet no AI credit can pay. */
    outOfCredits?: boolean;
  } | null;
};

/** Whether the account's purchased credits pay for model requests now, as its summary says; without that word,
 * the older rule: only with overage on, and never while on hold. */
export function purchasedPays(s: NonNullable<BillingState["summary"]>): boolean {
  return s.purchasedPays ?? (s.overageOptIn && !s.purchasedFrozen);
}

/** Whether Haarness Cloud models can be paid for, as far as `state` says: false only for a signed-in account
 * whose summary shows no credit that can pay (nothing included left, and purchased credits none, waiting on
 * overage on a plan, or on hold), or that is not active. Signed out or still loading, nothing is known. */
export function cloudCanPay(state: BillingState): boolean {
  const s = state.summary;
  if (!state.loggedIn || !s) return true;
  const some = (value: string) => /[1-9]/.test(value);
  return (
    s.accountStatus === "active" &&
    (some(s.includedCredits) || (purchasedPays(s) && !s.purchasedFrozen && some(s.purchasedCredits)))
  );
}

/** The credits that can pay now, for a glance: included ones, plus purchased ones while they pay. */
export function spendableCredits(s: NonNullable<BillingState["summary"]>): string {
  return purchasedPays(s) && !s.purchasedFrozen ? addCredits(s.includedCredits, s.purchasedCredits) : s.includedCredits;
}

/** Purchased credits sit unspent until overage is turned on: on a plan with overage off. Without a plan they pay
 * as they are, and on hold overage would not help. */
export function waitsForOverage(s: NonNullable<BillingState["summary"]>): boolean {
  return !purchasedPays(s) && !s.overageOptIn && !s.purchasedFrozen && /[1-9]/.test(s.purchasedCredits);
}

/** The English key naming the purchased balance: a new account's free credits until it buys some. */
export function purchasedLabel(s: NonNullable<BillingState["summary"]>): string {
  return s.purchasedIsStarter ? "Starter credits" : "Purchased credits";
}

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
