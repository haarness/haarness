import { t } from "./i18n";
import { Show } from "solid-js";
import { useApp } from "./state";
import { Button, ErrorWords, Icon } from "./ui";
import { errorText } from "./error-text";

/** Render in the active surface so native modality cannot hide operation errors. */
export function Notifications() {
  const { state, set } = useApp();
  return (
      <div class="notifications">
        <Show when={state.notice}>
          <div class="toast" role="status" data-tone={state.noticeTone}>
            <Icon name={state.noticeTone === "blocked" ? "alert" : "check"} size="sm" />
            {/* Callers pass an English key or an already translated sentence. */}
            {t(state.notice)}
          </div>
        </Show>
        <Show when={errorText(state.error).message}>
          <div class="error-banner" role="alert">
            <Icon name="alert" size="sm" />
            <span title={errorText(state.error).detail}>
              <ErrorWords error={state.error} />
            </span>
            <Button
              icon="close"
              label={t("Dismiss error")}
              onClick={() => set("error", "")}
            />
          </div>
        </Show>
      </div>
  );
}
