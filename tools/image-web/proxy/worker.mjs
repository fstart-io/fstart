// A deliberately narrow CORS bridge, not a general URL-fetching proxy.
const DEFAULT_REPOSITORY = "fstart-io/fstart";
const component = /^[A-Za-z0-9][A-Za-z0-9._-]{0,191}$/;

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
    if (url.search || parts.length !== 2 || parts.some(p => !component.test(p) || p.includes(".."))) return reply("Expected /tag/asset", 400);
    if (request.method === "OPTIONS") return reply(null, 204);
    if (!["GET", "HEAD"].includes(request.method)) return reply("Method not allowed", 405);
    const repository = env.REPOSITORY || DEFAULT_REPOSITORY;
    if (!/^[A-Za-z0-9_-]+\/[A-Za-z0-9_.-]+$/.test(repository)) return reply("Invalid repository configuration", 500);
    let target = new URL(`https://github.com/${repository}/releases/download/${parts[0]}/${parts[1]}`);
    const requestHeaders = new Headers({ "User-Agent": "fstart-release-proxy", Accept: "application/octet-stream" });
    const range = request.headers.get("Range");
    if (range) {
      if (!/^bytes=\d*-\d*$/.test(range) || range === "bytes=-") return reply("Invalid range", 400);
      requestHeaders.set("Range", range);
    }
    try {
      for (let hop = 0; hop < 4; hop++) {
        const upstream = await upstreamFetch(target.href, { method: request.method, headers: requestHeaders, redirect: "manual", signal: request.signal });
        if ([301, 302, 303, 307, 308].includes(upstream.status)) {
          const location = upstream.headers.get("Location");
          await upstream.body?.cancel();
          if (!location) return reply("Missing upstream redirect", 502);
          target = new URL(location, target);
          if (target.protocol !== "https:" || target.username || target.password || target.hostname !== "release-assets.githubusercontent.com") return reply("Unexpected upstream redirect", 502);
          continue;
        }
        if (![200, 206, 304].includes(upstream.status)) {
          await upstream.body?.cancel();
          return reply("Release asset unavailable", upstream.status === 404 ? 404 : 502);
        }
        for (const name of ["Content-Type", "Content-Length", "Content-Range", "Accept-Ranges", "ETag", "Last-Modified", "Content-Disposition"]) {
          const value = upstream.headers.get(name);
          if (value) headers.set(name, value);
        }
        headers.set("Cache-Control", "public, max-age=3600");
        headers.set("X-Content-Type-Options", "nosniff");
        return new Response(request.method === "HEAD" ? null : upstream.body, { status: upstream.status, headers });
      }
      return reply("Too many upstream redirects", 502);
    } catch {
      return reply("Upstream download failed", 502);
    }
  };
}

export default { fetch: createHandler() };
