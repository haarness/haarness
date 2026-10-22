import { createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import type * as T from "@haarness/sdk";
import type { useApp } from "./state";
import { t } from "./i18n";
import { modelName } from "../model-names";
import {
  macOnlyControls,
  settingsControls,
  settingsTitles,
  settingsKeywords,
  shortcutTitles,
} from "./settings-catalog";
import { shortcutLabel, type SearchEntry } from "./search";
/**
 * Settings search, in two depths.
 *
 * The Settings dialog's own filter searches everything a settings page can
 * open — every installed model, tool, hook and extension — because the reader
 * is already inside Settings and looking for one of them.
 *
 * Universal search asks a different question, so it takes `inventory: false`
 * and stays at the level a reader thinks in: settings sections, the individual
 * controls, and shortcuts. Listing the installed catalog there buried chats,
 * files and commands under hundreds of rows nobody opens the palette to find,
 * and cost a round of catalog requests on every connection change.
 */
export function createSettingsSearch(
  app: ReturnType<typeof useApp>,
  { inventory = true }: { inventory?: boolean } = {},
) {
  const [catalog, setCatalog] = createSignal<SearchEntry[]>([]);
  const entry = (
    section: string,
    name: string,
    target?: string,
    keywords = "",
    key?: string,
  ): SearchEntry => ({
    key: key ?? `setting:${section}:${target ?? "section"}`,
    group: "Settings",
    name: t(name),
    icon: "settings",
    detail: `${t("Settings")} › ${t(settingsTitles[section])}`,
    keywords: name + " " + keywords,
    destination: "settings",
    action: () => app.openSettings(section, target),
  });
  createEffect(() => {
    if (!inventory) return;
    const connection = app.state.connection?.id,
      remote = app.state.connection?.remote;
    app.state.catalogVersion;
    let disposed = false;
    setCatalog([]);
    if (!connection) return;
    const load = async () => {
      const results = await Promise.allSettled([
        window.haarness.api<T.SkillInfo[]>(connection, "skills", []),
        window.haarness.api<T.McpServerInfo[]>(connection, "mcpServers", []),
        window.haarness.api<T.PluginInfo[]>(connection, "plugins", []),
        window.haarness.api<T.ToolCatalogResponse>(connection, "toolCatalog", [
          {},
        ]),
        ...["hooks", "schedules"].map((resource) =>
          window.haarness.api<T.ManagementView>(connection, "manageResource", [
            resource,
            { action: "view" },
          ]),
        ),
      ] as const);
      if (disposed) return;
      const entries: SearchEntry[] = [];
      const [skills, servers, plugins, tools, hooks, schedules] = results;
      if (skills.status === "fulfilled")
        for (const item of skills.value)
          entries.push(
            entry("skills", item.name, "skills:" + item.name, item.description),
          );
      if (servers.status === "fulfilled")
        for (const item of servers.value)
          entries.push(
            entry(
              "mcp",
              item.name,
              "mcp:" + item.name,
              "MCP server integration",
            ),
          );
      if (tools.status === "fulfilled")
        for (const item of tools.value.tools)
          entries.push(
            entry("tools", item.id, "tool:" + item.id, item.description),
          );
      if (plugins.status === "fulfilled")
        for (const item of plugins.value)
          entries.push(
            entry(
              "plugins",
              item.name,
              "plugin:" + item.name,
              item.description,
            ),
          );
      for (const [section, result] of [
        ["hooks", hooks],
        ["schedules", schedules],
      ] as const) {
        if (result?.status !== "fulfilled") continue;
        for (const item of result.value.entries ?? []) {
          const request = item.request as Record<string, unknown>;
          if (request.action === "inspect" && typeof request.id === "string")
            entries.push(
              entry(
                section,
                item.label,
                section + ":" + request.id,
                item.description,
              ),
            );
        }
      }
      setCatalog(entries);
      if (!remote) {
        const extensions = await Promise.allSettled(
          [false, true].map((project) =>
            window.haarness.native<{
              entries: { name?: string; source: string }[];
            }>(connection, { action: "extensions", project }),
          ),
        );
        if (disposed) return;
        for (const [index, result] of extensions.entries()) {
          if (result.status !== "fulfilled") continue;
          for (const item of result.value.entries ?? [])
            entries.push(
              entry(
                "extensions",
                item.name || item.source,
                "extension:" +
                  (index ? "project" : "global") +
                  ":" +
                  item.source,
                `${item.source} ${index ? "project" : "global"}`,
                "extension:" + index + ":" + item.source,
              ),
            );
        }
        setCatalog([...entries]);
      }
    };
    void load();
    onCleanup(() => {
      disposed = true;
    });
  });
  return createMemo(() => [
    ...Object.entries(settingsTitles).map(([section, title]) =>
      entry(section, title, undefined, settingsKeywords[section]),
    ),
    ...settingsControls
      .filter(([, title]) => app.state.platform === "darwin" || !macOnlyControls.has(title))
      .map(([section, title, keywords]) => entry(section, title, title, keywords)),
    // "Shortcut: New chat" is one sentence in the catalog, so a language
    // can order the two words its own way. Search still matches the English.
    ...Object.entries(app.state.preferences.shortcuts).map(([name, value]) => ({
      ...entry(
        "shortcuts",
        "Shortcut: " + (shortcutTitles[name] ?? name),
        "shortcut:" + name,
        `${value} keyboard hotkey keybind customize`,
      ),
      name: t("Shortcut: {name}", { name: t(shortcutTitles[name] ?? name) }),
      group: "Shortcuts",
      hint: shortcutLabel(value, app.state.platform),
    })),
    ...[
      ["New line", "Shift+Enter"],
      ["Save file", "Mod+S"],
      ["Find in file", "Mod+F"],
      ["Close dialog", "Escape"],
    ].map(([name, value]) => ({
      ...entry("shortcuts", "Shortcut: " + name, name, value),
      name: t("Shortcut: {name}", { name: t(name) }),
      group: "Shortcuts",
      hint: shortcutLabel(value, app.state.platform),
    })),
    // Providers stay in universal search even when the installed catalog does
    // not: a short, recognisable list carrying brand artwork, and the way in to
    // connecting a model. Their models are inventory and stay behind Settings.
    ...app.state.providers.map((provider) => ({
      ...entry("providers", provider.name, "provider:" + provider.id, provider.id),
      provider: provider.id,
    })),
    ...(inventory
      ? [
          // Your other computers are found as "Your computers", which lists
          // only the ones the account still has.
          ...app
            .servers()
            .filter((server) => !server.host)
            .map((server) =>
              entry(
                "servers",
                server.label,
                "server:" + server.id,
                server.url ?? "local computer",
              ),
            ),
          ...app
            .projects()
            .map((project) =>
              entry(
                "projects",
                project.label,
                "project:" + project.id,
                project.root,
              ),
            ),
          ...app.state.providers.flatMap((provider) =>
            provider.models.map((model) => ({
              ...entry(
                "models",
                modelName(provider.id, model.id, app.state.providers),
                "model:" + provider.id + ":" + model.id,
                model.id + " " + provider.name,
              ),
              provider: provider.id,
            })),
          ),
          ...app.state.agents
            .filter((agent) => !agent.hidden)
            .map((agent) =>
              entry(
                "agents",
                agent.id,
                "agents:" + agent.id,
                agent.description,
              ),
            ),
          ...app.state.commands.map((command) =>
            entry(
              "commands",
              "Configure /" + command.name,
              "commands:" + command.name,
              command.description,
            ),
          ),
          ...catalog(),
        ]
      : []),
  ]);
}
