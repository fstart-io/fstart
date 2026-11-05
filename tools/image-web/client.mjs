// Release discovery and verified downloads. No UI or framework assumptions:
// the web UI (www/) and the Node tests both use these helpers.
export const repository = "fstart-io/fstart";
export const githubApi = "https://api.github.com";
export const bundleFormatVersion = 2;

// GitHub's API allows CORS; release asset downloads do not, so those go
// through the release proxy (proxy/worker.mjs).
export async function releases({ fetch: request = fetch, api = githubApi, repo = repository } = {}) {
  const response = await request(`${api.replace(/\/$/, "")}/repos/${repo}/releases?per_page=100`, {
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

export async function sha256(bytes) {
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(hash, b => b.toString(16).padStart(2, "0")).join("");
}

// Reads the body in chunks so a UI can show progress; `onProgress(received,
// total)` is called after every chunk. The result must match the catalog's
// size and SHA-256 before anyone parses it.
export async function bundle(proxy, tag, build, { fetch: request = fetch, onProgress } = {}) {
  const response = await request(assetURL(proxy, tag, build.asset));
  if (!response.ok) throw new Error(`Bundle download failed (${response.status})`);
  const bytes = await readAll(response, build.size, onProgress);
  if (bytes.length !== build.size) throw new Error("Bundle length mismatch");
  if (await sha256(bytes) !== build.sha256) throw new Error("Bundle checksum mismatch");
  return bytes;
}

// The vendor image a build's catalog entry names (build.donor_source),
// fetched through the proxy's /donor route and checked like a bundle. Only
// call this after the user accepted the vendor's terms (donor_source.terms).
export async function donor(proxy, tag, build, { fetch: request = fetch, onProgress } = {}) {
  const source = build.donor_source;
  if (!source) throw new Error("This build names no vendor download");
  const url = assetURL(proxy, tag, build.board);
  url.pathname = url.pathname.replace(/[^/]+\/[^/]+$/, `donor/${encodeURIComponent(tag)}/${encodeURIComponent(build.board)}`);
  const response = await request(url);
  if (!response.ok) throw new Error(`Vendor download failed (${response.status})`);
  const bytes = await readAll(response, source.size, onProgress);
  if (bytes.length !== source.size) throw new Error("Vendor download length mismatch");
  if (await sha256(bytes) !== source.sha256) throw new Error("Vendor download checksum mismatch: the vendor may have replaced the file");
  return bytes;
}

async function readAll(response, expected, onProgress) {
  if (!response.body?.getReader) return new Uint8Array(await response.arrayBuffer());
  const reader = response.body.getReader();
  const chunks = [];
  let received = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    received += value.length;
    // A proxy or server must not make the browser buffer more than the
    // catalog promised.
    if (received > expected) {
      await reader.cancel();
      throw new Error("Download is larger than expected");
    }
    chunks.push(value);
    onProgress?.(received, expected);
  }
  const bytes = new Uint8Array(received);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
}
