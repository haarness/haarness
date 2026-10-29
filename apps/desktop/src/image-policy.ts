/**
 * Images on the Desktop page come from the app itself and, for marketplace
 * icons, from Haarness Cloud: production, and the https Cloud named in the
 * environment when the page is built or served in development
 * (`HAARNESS_CLOUD_API_URL`, `HAARNESS_CLOUD_WEB_ORIGIN`). Only `img-src`
 * changes; every other directive stays as `index.html` has it.
 */
export const IMAGE_POLICY = "img-src 'self' data: blob: https://haarness.ai";

export function withCloudImages(html: string, env: Record<string, string | undefined>) {
  if (!html.includes(IMAGE_POLICY + ";")) throw new Error("index.html no longer has the expected img-src policy.");
  const origins = new Set<string>();
  for (const value of [env.HAARNESS_CLOUD_API_URL, env.HAARNESS_CLOUD_WEB_ORIGIN]) {
    try {
      const url = new URL(value ?? "");
      if (url.protocol === "https:" && !url.username && !url.password) origins.add(url.origin);
    } catch {}
  }
  origins.delete("https://haarness.ai");
  return origins.size ? html.replace(IMAGE_POLICY, [IMAGE_POLICY, ...origins].join(" ")) : html;
}
