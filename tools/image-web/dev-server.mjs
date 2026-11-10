// Local preview of the web UI against a local `fbuild bundle` output.
//
//   node tools/image-web/dev-server.mjs SITE_DIR DIST_DIR [PORT]
//
// SITE_DIR is target/image-web (ci/build-distribution.sh), DIST_DIR holds
// catalog.json and the bundles. The server stands in for both GitHub's
// release API (one release named "local") and the release proxy; proxy
// requests go through the real worker.mjs handler, including vendor donor
// downloads for boards whose catalog entry names one.
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { createHandler } from "./proxy/worker.mjs";

const [site, dist, port = "8080"] = process.argv.slice(2);
if (!site || !dist) {
  console.error("usage: dev-server.mjs SITE_DIR DIST_DIR [PORT]");
  process.exit(2);
}
const origin = `http://localhost:${port}`;
const TAG = "local";
const types = { ".html": "text/html", ".mjs": "text/javascript", ".js": "text/javascript", ".css": "text/css", ".json": "application/json", ".wasm": "application/wasm" };

// Release downloads come from DIST_DIR; vendor downloads (the /donor route)
// go to the real vendor, as they do from the deployed worker.
const proxy = createHandler(async (url, options) => {
  if (!url.startsWith("https://github.com/")) return fetch(url, options);
  const asset = decodeURIComponent(new URL(url).pathname.split("/").pop());
  const file = path.join(dist, path.basename(asset));
  if (!fs.existsSync(file)) return new Response("missing", { status: 404 });
  const data = fs.readFileSync(file);
  return new Response(data, { status: 200, headers: { "Content-Length": String(data.length), "Content-Type": "application/octet-stream" } });
});

function release() {
  const catalog = JSON.parse(fs.readFileSync(path.join(dist, "catalog.json")));
  return {
    tag_name: TAG,
    name: `Local build ${catalog.revision?.slice(0, 12) ?? ""}`,
    draft: false,
    prerelease: true,
    published_at: new Date().toISOString(),
    assets: [{ name: "catalog.json" }, ...catalog.builds.map(b => ({ name: b.asset }))],
  };
}

http.createServer(async (req, res) => {
  const url = new URL(req.url, origin);
  try {
    if (url.pathname === "/config.json") {
      // Relative, so the page works under whatever host serves it (e.g. an
      // HTTPS reverse proxy for testing on a phone).
      return send(res, 200, "application/json", JSON.stringify({ proxy: "/proxy/", api: "/api" }));
    }
    if (url.pathname.endsWith("/releases") && url.pathname.startsWith("/api/repos/")) {
      return send(res, 200, "application/json", JSON.stringify([release()]));
    }
    if (url.pathname.startsWith("/proxy/")) {
      const request = new Request(new URL(url.pathname.slice("/proxy".length), origin), {
        method: req.method,
        headers: { Origin: origin, ...(req.headers.range ? { Range: req.headers.range } : {}) },
      });
      // The page is same-origin with this server, whatever its host name.
      const response = await proxy(request, { ALLOWED_ORIGINS: origin });
      res.writeHead(response.status, Object.fromEntries(response.headers));
      if (!response.body) return res.end();
      // Stream, like the deployed worker, so the UI's progress bars move.
      for await (const chunk of response.body) res.write(chunk);
      return res.end();
    }
    const root = path.resolve(site);
    let file = path.resolve(root, `.${path.posix.normalize(decodeURIComponent(url.pathname))}`);
    if (file !== root && !file.startsWith(root + path.sep)) return send(res, 403, "text/plain", "forbidden");
    if (fs.existsSync(file) && fs.statSync(file).isDirectory()) file = path.join(file, "index.html");
    if (!fs.existsSync(file)) return send(res, 404, "text/plain", "not found");
    return send(res, 200, types[path.extname(file)] ?? "application/octet-stream", fs.readFileSync(file));
  } catch (error) {
    return send(res, 500, "text/plain", String(error));
  }
}).listen(Number(port), () => console.log(`fstart web UI: ${origin}/`));

function send(res, status, type, body) {
  res.writeHead(status, { "Content-Type": type });
  res.end(body);
}
