/**
 * Images on the Desktop page come from the app itself and, for marketplace
 * icons, from Haarness Cloud: production, and the https Cloud named in the
 * environment (`HAARNESS_CLOUD_API_URL`, `HAARNESS_CLOUD_WEB_ORIGIN`) when
 * the page is built or served in development. Only `img-src` changes; every
 * other directive stays as `index.html` has it.
 */
export const IMAGE_POLICY = "img-src 'self' data: blob: https://haarness.ai";

type Env = Record<string, string | undefined>;

/** Haarness Cloud's https origins: production first, then any the environment names. */
export function cloudImageOrigins(env: Env) {
  const origins = new Set(["https://haarness.ai"]);
  for (const value of [env.HAARNESS_CLOUD_API_URL, env.HAARNESS_CLOUD_WEB_ORIGIN]) {
    try {
      const url = new URL(value ?? "");
      if (url.protocol === "https:" && !url.username && !url.password) origins.add(url.origin);
    } catch {}
  }
  return [...origins];
}

export function withCloudImages(html: string, env: Env) {
  if (!html.includes(IMAGE_POLICY + ";")) throw new Error("index.html no longer has the expected img-src policy.");
  const extra = cloudImageOrigins(env).slice(1);
  return extra.length ? html.replace(IMAGE_POLICY, [IMAGE_POLICY, ...extra].join(" ")) : html;
}
