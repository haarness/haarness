import { createSignal } from "solid-js";
import { cloudCanPay, type BillingState } from "../billing";

const [state, setState] = createSignal<BillingState>({ loggedIn: false });
let listening = false;

/** The signed-in account's billing as main watches it, for choices made
 * outside the account dialog. The first read starts following it. */
function billing(): BillingState {
  if (!listening) {
    listening = true;
    void window.haarness
      .account({ action: "billing-state" })
      .then((value: BillingState) => setState(value))
      .catch(() => {});
    window.haarness.onEvent((event) => {
      if (event.kind !== "menu") return;
      const data = event.data as { action?: string; state?: BillingState };
      if (data.action === "account-billing" && data.state) setState(data.state);
    });
  }
  return state();
}

/** Providers on this computer whose account cannot pay now: Haarness Cloud,
 * while the signed-in account has no credit that can pay. A remote server's
 * Cloud provider is its own account's, which this one says nothing about.
 * Follows billing, so a trial started in the browser puts Cloud back. */
export function unpaidProviders(remote: boolean | undefined): string[] {
  return !remote && !cloudCanPay(billing()) ? ["haarness"] : [];
}
