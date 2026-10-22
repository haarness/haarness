import { t } from "./i18n";
import { render } from "solid-js/web";
import { ErrorBoundary } from "solid-js";
import { App } from "./App";
import { AppProvider } from "./state";
import "./base.css";
import "./shell.css";
import "./chat.css";
import "./panels.css";
import "./overlay.css";
// The saved settings are read before the first render, so the window opens
// in its own theme, palette, text size and language rather than flashing the
// defaults until the rest of the app's state has loaded. One read, from the
// main process's memory; a main process busy elsewhere (a native dialog in
// another window) does not leave this window empty: after 150 ms it starts
// from the defaults, as it did before. Until then the window shows its own
// background, the saved theme's page colour (base.css).
const preferences = await Promise.race([
  window.haarness.boot().then(
    (value) => value.preferences,
    () => undefined,
  ),
  new Promise<undefined>((resolve) => setTimeout(resolve, 150)),
]);
render(
  () => (
    <ErrorBoundary
      fallback={(error, reset) => {
        console.error("Desktop view failed", error);
        return (
          <main class="fatal">
            <h1>{t("The window needs a fresh start.")}</h1>
            <p>{t("Your server keeps running.")}</p>
            <p>{t("Reload the window to continue.")}</p>
            <button onClick={reset}>{t("Try again")}</button>
            <button onClick={() => location.reload()}>{t("Reload window")}</button>
          </main>
        );
      }}
    >
      <AppProvider preferences={preferences}>
        <App />
      </AppProvider>
    </ErrorBoundary>
  ),
  document.getElementById("root")!,
);
