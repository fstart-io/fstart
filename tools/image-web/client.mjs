// Headless frontend integration. No UI or framework assumptions.
export const repository = "fstart-io/fstart";
export const bundleFormatVersion = 2;

export async function releases({ fetch: request = fetch } = {}) {
  const response = await request(`https://api.github.com/repos/${repository}/releases?per_page=100`, {
    headers: { Accept: "application/vnd.github+json" },
  });
  if (!response.ok) throw new Error(`GitHub release discovery failed (${response.status})`);
  return (await response.json()).filter(release => !release.draft && release.assets.some(a => a.name === "catalog.json"));
}

export function assetURL(proxy, tag, asset) {
  if (![tag, asset].every(s => /^[A-Za-z0-9][A-Za-z0-9._-]{0,191}$/.test(s) && !s.includes(".."))) throw new Error("Invalid release asset identity");
  const url = new URL(proxy);
  if (url.protocol !== "https:" && !(url.protocol === "http:" && ["localhost", "127.0.0.1"].includes(url.hostname))) throw new Error("Proxy must use HTTPS");
  return new URL(`${encodeURIComponent(tag)}/${encodeURIComponent(asset)}`, url.href.replace(/\/?$/, "/"));
}

export async function catalog(proxy, tag, { fetch: request = fetch } = {}) {
  const response = await request(assetURL(proxy, tag, "catalog.json"));
  if (!response.ok) throw new Error(`Catalog download failed (${response.status})`);
  const value = await response.json();
  if (value.format_version !== bundleFormatVersion || !Array.isArray(value.builds)) throw new Error("Unsupported catalog format");
  return value;
}

export async function bundle(proxy, tag, build, { fetch: request = fetch } = {}) {
  const response = await request(assetURL(proxy, tag, build.asset));
  if (!response.ok) throw new Error(`Bundle download failed (${response.status})`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.length !== build.size) throw new Error("Bundle length mismatch");
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  const hex = Array.from(hash, b => b.toString(16).padStart(2, "0")).join("");
  if (hex !== build.sha256) throw new Error("Bundle checksum mismatch");
  return bytes;
}
