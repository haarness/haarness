import { Show, type ParentProps } from "solid-js";
import { providerBrand } from "../provider-brands";
import { Icon, type IconSize } from "./icons";
import "./provider-icons.css";

// Static, bundled assets only. Never fetch a logo from a configured endpoint.
const assets = import.meta.glob<string>("../assets/providers/*.svg", {
  eager: true,
  query: "?url",
  import: "default",
});

export function ProviderIcon(props: {
  provider?: string | null;
  size?: IconSize;
}) {
  const brand = () => providerBrand(props.provider);
  const url = () => assets[`../assets/providers/${brand()}.svg`];
  return (
    <span
      class="provider-icon"
      data-provider-icon={brand() ?? "generic"}
      data-size={props.size ?? "md"}
      aria-hidden="true"
    >
      <Show
        when={url()}
        fallback={<Icon name="cloud" size={props.size ?? "md"} />}
      >
        <span
          class="provider-icon-mask"
          style={{ "mask-image": `url("${url()}")` }}
        />
      </Show>
    </span>
  );
}

export function ProviderLabel(
  props: ParentProps<{ provider?: string | null; size?: IconSize }>,
) {
  return (
    <span class="provider-label">
      <ProviderIcon provider={props.provider} size={props.size} />
      <span>{props.children}</span>
    </span>
  );
}
