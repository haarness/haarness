import { t } from "./i18n";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  on,
  onCleanup,
  Show,
} from "solid-js";
import type { Turn, UsageResponse } from "@haarness/sdk";
import { useApp } from "./state";
import { Button } from "./ui";
import { ProviderLabel } from "./ProviderIcon";
import { modelName } from "../model-names";
import { contextUsage, tokenNumber } from "./context-usage";
import "./conversation-context.css";

export function ConversationContext() {
  const app = useApp();
  const thread = () =>
    app.state.threads.find((item) => item.id === app.state.selected);
  const context = createMemo(() =>
    thread() ? contextUsage(app.state.items, thread()!) : undefined,
  );
  const [totals, setTotals] = createSignal<UsageResponse>();
  const [lastTurn, setLastTurn] = createSignal<Turn | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [refresh, setRefresh] = createSignal(0);
  const latestId = () => context()?.latest?.id;
  const requestKey = createMemo(() =>
    JSON.stringify([
      app.state.connection?.id,
      app.state.selected,
      latestId(),
      refresh(),
    ]),
  );
  let generation = 0;
  createEffect(
    on(requestKey, () => {
      const connection = app.state.connection?.id;
      const id = app.state.selected;
      const turnId = context()?.latest?.turnId;
      const attempt = ++generation;
      setTotals(undefined);
      setLastTurn(null);
      setError("");
      if (!connection || !id) {
        setLoading(false);
        return;
      }
      setLoading(true);
      void Promise.all([
        window.haarness.api<UsageResponse>(connection, "threadUsage", [id]),
        turnId
          ? window.haarness
              .api<Turn>(connection, "getTurn", [turnId])
              .catch(() => null)
          : Promise.resolve(null),
      ])
        .then(([usage, turn]) => {
          if (attempt !== generation) return;
          setTotals(usage);
          setLastTurn(turn);
        })
        .catch((reason) => {
          if (attempt === generation)
            setError(
              reason instanceof Error ? reason.message : String(reason),
            );
        })
        .finally(() => {
          if (attempt === generation) setLoading(false);
        });
    }),
  );
  onCleanup(() => {
    generation++;
  });
  const model = () =>
    app.state.providers
      .find((provider) => provider.id === thread()?.provider)
      ?.models.find((model) => model.id === thread()?.model);
  const limit = () => model()?.contextWindow;
  /** The server's context number, measured as compaction measures it; shown
   * only when the model states its window. */
  const inUse = () => {
    const context = totals()?.context;
    return context?.window
      ? { ...context, window: context.window, percent: (context.tokens / context.window) * 100 }
      : undefined;
  };
  const percent = () =>
    limit() && context() ? (context()!.total / limit()!) * 100 : null;
  const latest = () => {
    const data = context()?.latest?.data;
    return data?.type === "usage" ? data : undefined;
  };
  const metrics = () =>
    [
      ["Input tokens", totals()?.inputTokens],
      ["Output tokens", totals()?.outputTokens],
      ["Cache read tokens", totals()?.cacheReadTokens],
      ["Cache write tokens", totals()?.cacheWriteTokens],
    ] as const;
  return (
    <section class="conversation-context" aria-label={t("Conversation context")}>
      <header>
        <h2>{t("Conversation context")}</h2>
        <Button
          icon="refresh"
          label={t("Refresh conversation context")}
          disabled={loading() || !thread()}
          onClick={() => setRefresh((value) => value + 1)}
        />
      </header>
      <Show
        when={thread()}
        fallback={
          <p>{t("Select a conversation to inspect its context and usage.")}</p>
        }
      >
        <p class="context-title">{thread()!.title}</p>
        <ProviderLabel provider={thread()!.provider}>
          {modelName(
            thread()!.provider,
            thread()!.model,
            app.state.providers,
          )}
        </ProviderLabel>
        <Show when={inUse()}>
          {(usage) => (
            <section aria-label={t("Context in use")}>
              <h3>{t("Context in use")}</h3>
              <div class="context-total">
                <strong data-testid="context-tokens">
                  {tokenNumber(usage().tokens)}
                </strong>
                <span>
                  {t("of {count} tokens", { count: tokenNumber(usage().window) })}
                </span>
              </div>
              <progress
                aria-label={t("Context in use")}
                max="100"
                value={Math.min(100, usage().percent)}
              />
              <small>
                {t("{percent}% of the context window", {
                  percent: usage().percent.toFixed(1),
                })}
              </small>
              <p class="context-note">
                {t("Last request reported by the provider plus an estimate of what was added since. Compacts automatically above {count} tokens.", { count: tokenNumber(usage().compactAt) })}
              </p>
            </section>
          )}
        </Show>
        <section aria-label={t("Retained context estimate")}>
          <h3>{t("Retained text estimate")}</h3>
          <div class="context-total">
            <strong data-testid="context-estimate">
              {tokenNumber(context()?.total)}
            </strong>
            <span>{t("estimated tokens")}</span>
          </div>
          <p>
            {limit()
              ? t("{count} model context limit", { count: tokenNumber(limit()) })
              : t("Model context limit unavailable")}
          </p>
          <Show when={percent() != null}>
            <progress
              aria-label={t("Estimated text share of context limit")}
              max="100"
              value={Math.min(100, percent()!)}
            />
            <small>
              {t("{percent}% of the model limit, before overhead", { percent: percent()!.toFixed(1) })}
            </small>
          </Show>
          <p class="context-note">
            {t("Approximation from saved text (4 characters per token), not a provider token count. Excludes system instructions, tool definitions, media and provider overhead.")}
          </p>
          <Show when={context()?.compacted}>
            <p class="context-note">
              {t("Uses the latest compaction summary and retained messages.")}
            </p>
          </Show>
          <Show when={context()?.attachments}>
            <p class="context-note">
              {t("{count} media attachments are not included in this estimate.", { count: context()!.attachments })}
            </p>
          </Show>
          <dl class="context-breakdown">
            <For each={context()?.breakdown}>
              {(row) => (
                <div>
                  <dt>{t(row.label)}</dt>
                  <dd>{tokenNumber(row.tokens)}</dd>
                </div>
              )}
            </For>
          </dl>
          <p class="context-note">
            {t("{count} retained user and assistant messages.", { count: context()?.messages ?? 0 })}{" "}
            {t("Undone history is excluded.")}
          </p>
        </section>
        <section aria-label={t("Latest request usage")}>
          <h3>{t("Latest reported request")}</h3>
          <Show when={latest()?.prompt}>
            {(prompt) => (
              <section aria-label={t("Prepared request estimate")}>
                <h4>{t("Prepared request estimate")}</h4>
                <dl class="context-breakdown">
                  <div><dt>{t("System instructions")}</dt><dd>{tokenNumber(prompt().system_tokens)}</dd></div>
                  <div><dt>{t("Tool definitions")}</dt><dd>{tokenNumber(prompt().tool_tokens)}</dd></div>
                  <div><dt>{t("Conversation history")}</dt><dd>{tokenNumber(prompt().history_tokens)}</dd></div>
                  <Show when={prompt().media_tokens}>
                    <div><dt>{t("Media")}</dt><dd>{tokenNumber(prompt().media_tokens)}</dd></div>
                  </Show>
                  <div><dt>{t("Total estimated tokens")}</dt><dd>{tokenNumber(prompt().system_tokens + prompt().tool_tokens + prompt().history_tokens + (prompt().media_tokens ?? 0))}</dd></div>
                </dl>
                <p class="context-note">
                  {prompt().media_tokens || !prompt().media_attachments
                    ? t("Snapshot after request hooks, estimated with this model family's token weights. Excludes provider framing. This describes the last request, not unsent changes.")
                    : t("Snapshot after request hooks, estimated at 4 characters per token. Excludes provider framing and tokens for {count} media attachments. This describes the last request, not unsent changes.", { count: prompt().media_attachments })}
                </p>
              </section>
            )}
          </Show>
          <Show
            when={latest()}
            fallback={
              <p class="context-note">
                {t("No usage report for the current retained context yet.")}
              </p>
            }
          >
            <p class="context-note">
              {lastTurn()
                ? modelName(
                    lastTurn()!.provider,
                    lastTurn()!.model,
                    app.state.providers,
                  )
                : t("Request model not available")}
            </p>
            <dl class="context-stats">
              <div>
                <dt>{t("Input")}</dt>
                <dd>{tokenNumber(latest()?.input_tokens)}</dd>
              </div>
              <div>
                <dt>{t("Output")}</dt>
                <dd>{tokenNumber(latest()?.output_tokens)}</dd>
              </div>
              <div>
                <dt>{t("Cache read")}</dt>
                <dd>{tokenNumber(latest()?.cache_read_tokens ?? 0)}</dd>
              </div>
              <div>
                <dt>{t("Cache write")}</dt>
                <dd>{tokenNumber(latest()?.cache_write_tokens ?? 0)}</dd>
              </div>
            </dl>
            <p class="context-note">
              {t("As reported by the provider. Cache tokens may overlap input tokens; these values are not added together. Reasoning token counts are not reported separately.")}
            </p>
          </Show>
        </section>
        <section aria-label={t("Conversation usage totals")} aria-busy={loading()}>
          <h3>{t("Conversation totals")}</h3>
          <p class="context-note">
            {t("Cumulative reported usage across requests, including undone and compacted history. This is not the current context size.")}
          </p>
          <Show when={loading()}>
            <p role="status">{t("Loading usage…")}</p>
          </Show>
          <Show when={error()}>
            <p role="alert">{t("Could not load usage: {error}", { error: error() })}</p>
          </Show>
          <Show when={totals()}>
            <dl class="context-stats">
              <For each={metrics()}>
                {([label, value]) => (
                  <div>
                    <dt>{t(label)}</dt>
                    <dd>{tokenNumber(value)}</dd>
                  </div>
                )}
              </For>
              <div>
                <dt>{t("Known cost")}</dt>
                <dd>
                  {totals()?.costUsd == null
                    ? t("Not available")
                    : "$" + totals()!.costUsd!.toFixed(4)}
                </dd>
              </div>
            </dl>
            <p class="context-note">
              {t("Cost includes only requests with available pricing.")}
            </p>
          </Show>
        </section>
      </Show>
    </section>
  );
}
