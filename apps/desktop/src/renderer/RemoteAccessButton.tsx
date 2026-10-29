import { onMount } from "solid-js";
import { t } from "./i18n";
import { Button } from "./ui";
import { useApp } from "./state";
import { pressRemoteAccess, remoteAccess, watchRemoteAccess } from "./remote-access";
import "./remote-control.css";

/**
 * The title bar's remote access control: one press makes this whole
 * computer reachable from the Haarness app on a phone and from any browser
 * signed in to the same account, another press ends that. The dot says it
 * is on (green), on its way (pulsing) or stopped with a problem (red).
 */
export function RemoteAccessButton() {
  const app = useApp();
  onMount(watchRemoteAccess);
  const state = () => {
    if (remoteAccess.busy === "on") return "connecting";
    if (remoteAccess.busy === "off") return "stopping";
    const access = remoteAccess.access;
    if (access?.error) return "error";
    return access?.state ?? "off";
  };
  const description = () => {
    const access = remoteAccess.access;
    switch (state()) {
      case "connecting":
        return t("Connecting to Haarness Cloud…");
      case "stopping":
        return t("Stopping…");
      case "error":
        return access?.error;
      case "on": {
        const on = t("On. Your phone and any browser signed in to Haarness can open {computer}.", {
          computer: access?.computer ?? t("This computer"),
        });
        return access?.organization
          ? `${on} ${t("Members of {organization} can open it too.", { organization: access.organization })}`
          : on;
      }
      default:
        return undefined;
    }
  };
  return (
    <Button
      icon="remote"
      class={`remote-access ${state()}`}
      label={t("Remote access")}
      hint={description() ? undefined : t("Open this computer's projects from your phone or any browser")}
      active={state() === "on" || state() === "connecting"}
      dot={state() !== "off" && state() !== "signed_out" && state() !== "stopping"}
      description={description()}
      onClick={() => pressRemoteAccess(app.notify)}
    />
  );
}
