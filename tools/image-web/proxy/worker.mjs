// A deliberately narrow CORS bridge, not a general URL-fetching proxy.
//
//   GET /<tag>/<asset>          a release asset of the configured repository
//   GET /donor/<tag>/<board>    the vendor download that release's catalog
//                               names for <board> (donor_source.url)
//
// The second route fetches only URLs a release catalog of this repository
// lists; the browser checks the bytes against the catalog's SHA-256.
const DEFAULT_REPOSITORY = "fstart-io/fstart";
const component = /^[A-Za-z0-9][A-Za-z0-9._-]{0,191}$/;
const RELEASE_HOST = "release-assets.githubusercontent.com";
const PASS_HEADERS = ["Content-Type", "Content-Length", "Content-Range", "Accept-Ranges", "ETag", "Last-Modified", "Content-Disposition"];

class Refusal extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

export function createHandler(upstreamFetch = fetch) {
  return async function handle(request, env = {}) {
    const origin = request.headers.get("Origin");
    const allowed = (env.ALLOWED_ORIGINS || "https://fstart-io.github.io").split(",").map(s => s.trim());
    const headers = new Headers({ Vary: "Origin" });
    if (origin && !allowed.includes(origin)) return new Response("Origin not allowed", { status: 403 });
    if (origin) headers.set("Access-Control-Allow-Origin", origin);
    headers.set("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
    headers.set("Access-Control-Allow-Headers", "Range");
    headers.set("Access-Control-Expose-Headers", "Content-Length, Content-Range, ETag");
    const reply = (text, status) => new Response(text, { status, headers });
    const url = new URL(request.url);
    const parts = url.pathname.slice(1).split("/");
    const donor = parts.length === 3 && parts[0] === "donor";
    const names = donor ? parts.slice(1) : parts;
    if (url.search || names.length !== 2 || names.some(p => !component.test(p) || p.includes(".."))) {
      return reply("Expected /tag/asset or /donor/tag/board", 400);
    }
    if (request.method === "OPTIONS") return reply(null, 204);
    if (!["GET", "HEAD"].includes(request.method)) return reply("Method not allowed", 405);
    const repository = env.REPOSITORY || DEFAULT_REPOSITORY;
    if (!/^[A-Za-z0-9_-]+\/[A-Za-z0-9_.-]+$/.test(repository)) return reply("Invalid repository configuration", 500);
    const range = request.headers.get("Range");
    if (range && (!/^bytes=\d*-\d*$/.test(range) || range === "bytes=-")) return reply("Invalid range", 400);
    const releaseAsset = asset => new URL(`https://github.com/${repository}/releases/download/${names[0]}/${asset}`);
    try {
      let target;
      let hosts;
      if (donor) {
        target = await donorURL(releaseAsset("catalog.json"), names[1], request.signal);
        // Vendor CDNs redirect within their own host at most.
        hosts = [target.hostname];
      } else {
        target = releaseAsset(names[1]);
        hosts = [RELEASE_HOST];
      }
      const upstream = await follow(target, hosts, { method: request.method, range, signal: request.signal });
      for (const name of PASS_HEADERS) {
        const value = upstream.headers.get(name);
        if (value) headers.set(name, value);
      }
      headers.set("Cache-Control", "public, max-age=3600");
      headers.set("X-Content-Type-Options", "nosniff");
      return new Response(request.method === "HEAD" ? null : upstream.body, { status: upstream.status, headers });
    } catch (error) {
      if (error instanceof Refusal) return reply(error.message, error.status);
      return reply("Upstream download failed", 502);
    }
  };

  // GET `target`, following up to four redirects to `hosts` (the first hop
  // may be github.com itself). Returns a 200/206/304 response.
  async function follow(target, hosts, { method = "GET", range, signal } = {}) {
    const requestHeaders = new Headers({ "User-Agent": "fstart-release-proxy", Accept: "application/octet-stream" });
    if (range) requestHeaders.set("Range", range);
    for (let hop = 0; hop < 5; hop++) {
      const upstream = await upstreamFetch(target.href, { method, headers: requestHeaders, redirect: "manual", signal });
      if ([301, 302, 303, 307, 308].includes(upstream.status)) {
        const location = upstream.headers.get("Location");
        await upstream.body?.cancel();
        if (!location) throw new Refusal("Missing upstream redirect", 502);
        target = new URL(location, target);
        if (target.protocol !== "https:" || target.username || target.password || !hosts.includes(target.hostname)) {
          throw new Refusal("Unexpected upstream redirect", 502);
        }
        continue;
      }
      if (![200, 206, 304].includes(upstream.status)) {
        await upstream.body?.cancel();
        throw new Refusal("Upstream file unavailable", upstream.status === 404 ? 404 : 502);
      }
      return upstream;
    }
    throw new Refusal("Too many upstream redirects", 502);
  }

  // The donor URL a release's catalog lists for `board`.
  async function donorURL(catalogURL, board, signal) {
    const response = await follow(catalogURL, [RELEASE_HOST], { signal });
    let catalog;
    try {
      catalog = JSON.parse(await response.text());
    } catch {
      throw new Refusal("Release catalog is not JSON", 502);
    }
    const source = catalog?.builds?.find(b => b.board === board && b.donor_source)?.donor_source;
    if (!source) throw new Refusal("No vendor download for this board", 404);
    let url;
    try {
      url = new URL(source.url);
    } catch {
      throw new Refusal("Invalid vendor download", 502);
    }
    if (url.protocol !== "https:" || url.username || url.password) throw new Refusal("Invalid vendor download", 502);
    return url;
  }
}

export default { fetch: createHandler() };
