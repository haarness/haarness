import { test } from "node:test";
import assert from "node:assert/strict";
import {
  browserSiteAccess,
  browserAddress,
  browserLinkDestination,
  isLocalBrowserURL,
} from "../src/browser-settings";
import { defaults } from "../src/shared";
import {
  settingsControls,
  settingsTitles,
  settingsKeywords,
} from "../src/renderer/settings-catalog";
import { searchEntries, type SearchEntry } from "../src/renderer/search";
test("link destination distinguishes loopback sites and non-web protocols", () => {
  for (const url of [
    "http://localhost:3000/a",
    "http://127.0.0.2/test",
    "http://[::1]:5173",
    "https://test.localhost",
  ]) {
    assert.equal(isLocalBrowserURL(url), true);
    assert.equal(browserLinkDestination(url, defaults), "browser");
  }
  for (const url of [
    "https://localhost.evil.test",
    "https://example.test",
    "mailto:hi@example.test",
    "https://127.0.0.1.evil.test",
  ]) {
    assert.equal(browserLinkDestination(url, defaults), "external");
  }
  assert.equal(
    browserLinkDestination("https://example.test", {
      browserWebLinks: "browser",
      browserLocalLinks: "external",
    }),
    "browser",
  );
  assert.equal(
    browserLinkDestination("file:///tmp/example", {
      browserWebLinks: "browser",
      browserLocalLinks: "browser",
    }),
    "external",
  );
});
test("URL display preserves actual navigation identity", () => {
  const url = "https://example.test:8443/path?q=hello#part";
  assert.equal(browserAddress(url, false), "https://example.test:8443");
  assert.equal(browserAddress(url, true), url);
  assert.equal(browserAddress("about:blank", false), "about:blank");
});
test("settings aliases cover browser and legacy controls using the global ranking algorithm", () => {
  const entries = settingsControls.map(
    ([section, name, keywords]): SearchEntry => ({
      key: section + name,
      name,
      keywords,
      group: "Settings",
      detail: settingsTitles[section],
      icon: "settings",
      action: () => {},
    }),
  );
  for (const [query, title] of [
    ["cookies", "Browsing data"],
    ["localhost", "Local URL destination"],
    ["permission notify", "Permission notifications"],
    ["terminal font", "Terminal text size"],
    ["restore archive", "Restore configuration"],
    ["disable automation", "Agent browser control"],
  ]) {
    assert.equal(searchEntries(entries, query)[0]?.name, title, query);
  }
  for (const [section, title] of settingsControls) {
    assert.ok(settingsTitles[section] && settingsKeywords[section]);
    assert.ok(
      searchEntries(entries, title).some((entry) => entry.name === title),
    );
  }
});

test("wildcard site rules preserve origin boundaries and prefer exact rules", () => {
  const allow = {browsing:"allow",downloads:"allow",uploads:"allow"} as const;
  const deny = {browsing:"deny",downloads:"deny",uploads:"deny"} as const;
  const policy={enabled:true,defaults:deny,sites:[{origin:"https://*.example.test",...allow},{origin:"https://private.example.test",...deny}]};
  for(const origin of ["https://a.example.test","https://a.b.example.test"]) assert.equal(browserSiteAccess(policy,origin).browsing,"allow");
  for(const origin of ["https://example.test","https://private.example.test","http://a.example.test","https://a.example.test:444","https://a.example.test.evil"]) assert.equal(browserSiteAccess(policy,origin).browsing,"deny");
});
