import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import {
  isRouterConnection,
  providerBrand,
  providerBrands,
} from "../src/provider-brands";

test("provider identity uses explicit aliases without misbranding compatible gateways", () => {
  assert.equal(providerBrand("anthropic"), "anthropic");
  assert.equal(providerBrand("gemini"), "gemini");
  assert.equal(providerBrand("google-vertex"), "vertexai");
  assert.equal(providerBrand("together"), providerBrand("togetherai"));
  assert.equal(providerBrand("fireworks-ai"), providerBrand("fireworks"));
  assert.equal(providerBrand("openrouter"), "openrouter");
  for (const value of [
    undefined,
    "",
    "openai-compatible",
    "company-openai-proxy",
    "https://anthropic.com",
    "constructor",
    "__proto__",
  ])
    assert.equal(providerBrand(value), undefined);
});

test("a connected Haarness Router is told apart from Haarness Cloud and other providers", () => {
  for (const id of ["router", "router_2", "router_12", " Router "])
    assert.equal(isRouterConnection(id), true, id);
  for (const id of [
    undefined,
    "",
    "haarness",
    "haarness-cloud",
    "openrouter",
    "router-proxy",
    "router_",
    "router_two",
    "my_router",
  ])
    assert.equal(isRouterConnection(id), false, String(id));
});

test("every registered brand ships a passive, locally attributed SVG", () => {
  const root = new URL("../src/assets/providers/", import.meta.url);
  const sources = JSON.parse(
    readFileSync(new URL("sources.json", root), "utf8"),
  ) as { file: string; source: string }[];
  for (const slug of new Set(Object.values(providerBrands))) {
    const file = new URL(slug + ".svg", root);
    assert.ok(existsSync(file), slug);
    const svg = readFileSync(file, "utf8");
    assert.match(svg, /viewBox=/);
    assert.doesNotMatch(
      svg,
      /<(?:script|foreignObject|image)\b|\bon\w+\s*=|(?:href|src)=["']https?:/i,
    );
    assert.ok(
      sources.some(
        (source) =>
          source.file === slug + ".svg" &&
          source.source.includes("raw.githubusercontent.com"),
      ),
    );
  }
});
