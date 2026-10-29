import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  cornersOpaque,
  iconFailed,
  iconFills,
  iconMeasured,
  iconSource,
  iconUnmeasured,
  rowsHaveIcons,
} from "../src/renderer/entry-icon";
import { IMAGE_POLICY, cloudImageOrigins, withCloudImages } from "../src/image-policy";

test("a view's icon loads only from an https address", () => {
  assert.equal(iconSource("https://haarness.ai/icons/a.png"), "https://haarness.ai/icons/a.png");
  for (const refused of [
    undefined,
    null,
    "",
    42,
    "/icons/a.png",
    "icons/a.png",
    "http://haarness.ai/icons/a.png",
    "data:image/png;base64,AAAA",
    "javascript:alert(1)",
    "file:///etc/hosts",
  ])
    assert.equal(iconSource(refused), undefined, String(refused));
});

test("an icon that failed to load is not tried again", () => {
  const icon = "https://haarness.ai/icons/broken.png";
  assert.equal(iconSource(icon), icon);
  iconFailed(icon);
  assert.equal(iconSource(icon), undefined);
  assert.equal(iconSource("https://haarness.ai/icons/other.png"), "https://haarness.ai/icons/other.png");
});

test("a list keeps an icon's width only when a row has one to show", () => {
  assert.equal(rowsHaveIcons([]), false);
  assert.equal(rowsHaveIcons([{}, { icon: null }]), false);
  assert.equal(rowsHaveIcons([{}, { icon: "https://haarness.ai/icons/list.png" }]), true);
  iconFailed("https://haarness.ai/icons/list.png");
  // Its one icon failed: the rows close up again.
  assert.equal(rowsHaveIcons([{}, { icon: "https://haarness.ai/icons/list.png" }]), false);
});

/** An RGBA square of `size` with every pixel at `alpha`, and `corner` alphas at the corners. */
function square(size: number, alpha: number, corner = alpha) {
  const data = new Uint8ClampedArray(size * size * 4).fill(255);
  for (let pixel = 0; pixel < size * size; pixel++) data[pixel * 4 + 3] = alpha;
  for (const pixel of [0, size - 1, (size - 1) * size, size * size - 1]) data[pixel * 4 + 3] = corner;
  return data;
}

test("an image fills its tile only when its corners are opaque", () => {
  assert.equal(cornersOpaque(square(8, 255), 8), true);
  // A logo on a transparent ground, even one that reaches the edges.
  assert.equal(cornersOpaque(square(8, 255, 0), 8), false);
  assert.equal(cornersOpaque(square(8, 0), 8), false);
  // Rounded corners blend into the ground: not a full square.
  assert.equal(cornersOpaque(square(8, 255, 200), 8), false);
  const one = square(8, 255);
  one[(8 * 8 - 1) * 4 + 3] = 0;
  assert.equal(cornersOpaque(one, 8), false);
});

test("a measured icon is remembered, and one that cannot be read keeps its margin", () => {
  const avatar = "https://haarness.ai/icons/avatar.png";
  const logo = "https://haarness.ai/icons/logo.png";
  const unreadable = "https://haarness.ai/icons/unreadable.png";
  for (const icon of [avatar, logo, unreadable]) assert.equal(iconUnmeasured(icon), true);
  assert.equal(iconFills(avatar), false);
  iconMeasured(avatar, true);
  iconMeasured(logo, false);
  iconMeasured(unreadable, undefined);
  assert.deepEqual([iconFills(avatar), iconFills(logo), iconFills(unreadable)], [true, false, false]);
  for (const icon of [avatar, logo, unreadable]) assert.equal(iconUnmeasured(icon), false);
});

const page = new URL("../src/renderer/index.html", import.meta.url);
const directives = (html: string) =>
  new Map(
    /http-equiv="Content-Security-Policy"\s+content="([^"]+)"/
      .exec(html)![1]
      .split(";")
      .map((part) => part.trim().split(/\s+/))
      .map(([name, ...values]) => [name, values]),
  );

test("the page loads images from Haarness Cloud and nothing else new", async () => {
  const html = await readFile(page, "utf8");
  assert.ok(html.includes(IMAGE_POLICY + ";"));
  const policy = directives(html);
  assert.deepEqual(policy.get("img-src"), ["'self'", "data:", "blob:", "https://haarness.ai"]);
  assert.deepEqual(policy.get("connect-src"), ["'none'"]);
  assert.deepEqual(policy.get("script-src"), ["'self'"]);
  assert.deepEqual(policy.get("default-src"), ["'self'"]);
  // Production alone, when no other Cloud is named.
  assert.deepEqual(cloudImageOrigins({}), ["https://haarness.ai"]);
  assert.equal(withCloudImages(html, {}), html);
  assert.equal(withCloudImages(html, { HAARNESS_CLOUD_API_URL: "https://haarness.ai/" }), html);
});

test("a build for another https Cloud adds its origins to img-src only", async () => {
  const html = await readFile(page, "utf8");
  const built = withCloudImages(html, {
    HAARNESS_CLOUD_API_URL: "https://staging.fixture.test/api/",
    HAARNESS_CLOUD_WEB_ORIGIN: "https://web.fixture.test:8443",
  });
  const before = directives(html);
  const after = directives(built);
  assert.deepEqual(after.get("img-src"), [
    ...before.get("img-src")!,
    "https://staging.fixture.test",
    "https://web.fixture.test:8443",
  ]);
  for (const [name, values] of before) if (name !== "img-src") assert.deepEqual(after.get(name), values, name);
  // A local Cloud on plain HTTP, or an address with credentials, adds nothing.
  assert.equal(
    withCloudImages(html, {
      HAARNESS_CLOUD_API_URL: "http://127.0.0.1:18080",
      HAARNESS_CLOUD_WEB_ORIGIN: "https://user:secret@web.fixture.test",
    }),
    html,
  );
  assert.equal(withCloudImages(html, { HAARNESS_CLOUD_API_URL: "not a url" }), html);
  assert.throws(() => withCloudImages(html.replace(IMAGE_POLICY, "img-src *"), {}));
});

// Rendering needs a DOM, which these tests do not have, so this reads the
// panel's source and styles: icons are decorative, lazy, on a light tile in
// every theme, and never left broken.
test("the panel draws icons as lazy decorative images on a light tile that vanish when they fail", async () => {
  const panel = await readFile(new URL("../src/renderer/ManagementPanel.tsx", import.meta.url), "utf8");
  const image = /<span class=\{props\.class\} classList=\{\{ loaded: loaded\(\), fills: fills\(src\) \}\}>\s*(<img\b[\s\S]*?\/>)/.exec(panel)?.[1];
  assert.ok(image, "the icon image on its tile");
  for (const attribute of ['alt=""', 'loading="lazy"', 'decoding="async"', 'referrerpolicy="no-referrer"'])
    assert.ok(image.includes(attribute), attribute);
  assert.match(image, /onError=\{\(\) => \{\s*iconFailed\(src\);\s*setBroken\(true\);/);
  assert.match(panel, /<Show when=\{!broken\(\) && iconSource\(props\.icon\)\} keyed>/);
  assert.match(panel, /<EntryIcon icon=\{current\.icon\} class="management-title-icon" \/>/);
  assert.match(panel, /<EntryIcon\s+icon=\{entry\.icon\}\s+class="management-icon"/);
  const styles = await readFile(new URL("../src/renderer/management.css", import.meta.url), "utf8");
  const theme = await readFile(new URL("../src/renderer/theme.css", import.meta.url), "utf8");
  assert.match(styles, /\.management-title-icon \{[^}]*background: var\(--icon-tile\);[^}]*background-clip: padding-box;[^}]*border: 1px solid var\(--hairline\);/);
  assert.match(styles, /\.management-title-icon > img \{[^}]*width: 78%;[^}]*object-fit: contain;/);
  assert.match(styles, /\.management-title-icon\.fills > img \{[^}]*width: 100%;/);
  // Fixed on :root, outside the palette every theme rewrites.
  assert.match(theme, /--icon-tile: #fdfcfa;/);
  const themes = await readFile(new URL("../src/themes.ts", import.meta.url), "utf8");
  assert.doesNotMatch(themes, /icon-tile|iconTile/);
});
