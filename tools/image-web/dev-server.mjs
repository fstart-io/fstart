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
import os from "node:os";
import { once } from "node:events";
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
// go to the real vendor, as they do from the deployed worker. Some vendor
// CDNs turn Node's fetch away by its TLS fingerprint (MiTAC's does), so with
// FSTART_DEV_CURL=1 this server fetches them with curl instead.
const vendor = process.env.FSTART_DEV_CURL === "1" ? curl : fetch;
const proxy = createHandler(async (url, options) => {
  if (!url.startsWith("https://github.com/")) return vendor(url, options);
  const asset = decodeURIComponent(new URL(url).pathname.split("/").pop());
  const file = path.join(dist, path.basename(asset));
  if (!fs.existsSync(file)) return new Response("missing", { status: 404 });
  const data = fs.readFileSync(file);
  return new Response(data, { status: 200, headers: { "Content-Length": String(data.length), "Content-Type": "application/octet-stream" } });
});

// A fetch through curl: the final response's status and headers, and its
// body as a stream that follows the reader's pace. Redirects are left to the
// caller, as with fetch's `redirect: "manual"`; `signal` stops the transfer.
// curl writes the header blocks to a file and only the body to stdout, so
// neither a proxy's CONNECT reply, a 1xx nor a trailer gets into the file.
async function curl(url, { method = "GET", headers = new Headers(), signal } = {}) {
  const { spawn } = await import("node:child_process");
  signal?.throwIfAborted();
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "fstart-curl-"));
  const headerFile = path.join(scratch, "headers");
  const args = [
    "--disable", "--globoff", "-sS", "--suppress-connect-headers", "-D", headerFile,
    // Fail a stalled transfer, not a long one.
    "--connect-timeout", "30", "--speed-limit", "1", "--speed-time", "60",
    "-X", method, url,
  ];
  // HEAD: the headers are only read from the dump, once curl is done.
  if (method === "HEAD") args.push("-I", "-o", os.devNull);
  for (const [name, value] of headers) args.push("-H", `${name}: ${value}`);
  const child = spawn("curl", args, { stdio: ["ignore", "pipe", "inherit"] });
  const stop = () => child.kill();
  signal?.addEventListener("abort", stop);
  let controller;
  let canceled = false;
  const body = new ReadableStream({
    start: c => { controller = c; },
    pull: () => { child.stdout.resume(); },
    cancel: () => { canceled = true; stop(); },
  });
  // The header file is complete once the body starts, or curl has exited.
  let first = null;
  const head = await new Promise((resolve, reject) => {
    let started = false;
    const begin = () => {
      if (started) return;
      started = true;
      try {
        resolve(fs.readFileSync(headerFile, "latin1"));
      } catch (error) {
        reject(error);
      }
    };
    child.stdout.on("data", chunk => {
      if (canceled) return;
      if (!started) {
        first = new Uint8Array(chunk);
        child.stdout.pause();
        return begin();
      }
      controller.enqueue(new Uint8Array(chunk));
      if (controller.desiredSize <= 0) child.stdout.pause();
    });
    child.on("error", reject);
    child.on("close", code => {
      signal?.removeEventListener("abort", stop);
      // No body (a redirect, a HEAD): the dump is read before it is removed.
      if (!started && code === 0) begin();
      fs.rmSync(scratch, { recursive: true, force: true });
      if (!started) return reject(new Error(`curl exited with ${code}`));
      if (canceled) return;
      // A transfer cut short is an error, not a short file.
      try {
        if (code === 0) controller.close();
        else controller.error(new Error(`curl exited with ${code}`));
      } catch {}
    });
  });
  // The last header block is the final response.
  const blocks = head.split(/\r\n\r\n/).filter(b => b.startsWith("HTTP/"));
  const [status, ...lines] = (blocks.at(-1) ?? "").split("\r\n");
  const response = new Headers();
  for (const line of lines) {
    const colon = line.indexOf(":");
    if (colon > 0) response.append(line.slice(0, colon).trim(), line.slice(colon + 1).trim());
  }
  const code = Number(status.split(" ")[1]);
  if (first) controller.enqueue(first);
  child.stdout.resume();
  const nullBody = method === "HEAD" || [101, 103, 204, 205, 304].includes(code);
  if (nullBody) body.cancel().catch(() => {});
  return new Response(nullBody ? null : body, { status: code, headers: response });
}

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
      // A browser that goes away stops the upstream transfer too.
      const gone = new AbortController();
      res.on("close", () => { if (!res.writableFinished) gone.abort(); });
      const request = new Request(new URL(url.pathname.slice("/proxy".length), origin), {
        method: req.method,
        headers: { Origin: origin, ...(req.headers.range ? { Range: req.headers.range } : {}) },
        signal: gone.signal,
      });
      // The page is same-origin with this server, whatever its host name.
      const response = await proxy(request, { ALLOWED_ORIGINS: origin });
      res.writeHead(response.status, Object.fromEntries(response.headers));
      if (!response.body) return res.end();
      // Stream, like the deployed worker, so the UI's progress bars move,
      // at the browser's pace. A transfer that fails midway ends the
      // connection rather than a short file.
      try {
        for await (const chunk of response.body) {
          if (!res.write(chunk)) await once(res, "drain", { signal: gone.signal });
        }
      } catch {
        return res.destroy();
      }
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
