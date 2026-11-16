import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHandler } from './worker.mjs';
const origin = 'https://fstart-io.github.io';
const request = (path = '/nightly-2026-01-01-abc/mj11.bundle.zip', init = {}) => new Request(`https://proxy.example${path}`, { headers: { Origin: origin }, ...init });

test('streams public release bytes through a validated redirect with CORS', async () => {
  const calls = [];
  const handler = createHandler(async (url, options) => {
    calls.push(url);
    assert.equal(options.redirect, 'manual');
    assert.equal(options.headers.get('Authorization'), null);
    if (calls.length === 1) return new Response(null, { status: 302, headers: { Location: 'https://release-assets.githubusercontent.com/a?signature=x' } });
    return new Response(new Uint8Array([1,2,3]), { headers: { 'Content-Type': 'application/zip', 'Content-Length': '3', 'Cross-Origin-Resource-Policy': 'same-origin' } });
  });
  const response = await handler(request());
  assert.equal(calls[0], 'https://github.com/fstart-io/fstart/releases/download/nightly-2026-01-01-abc/mj11.bundle.zip');
  assert.equal(response.headers.get('Access-Control-Allow-Origin'), origin);
  assert.equal(response.headers.get('Cross-Origin-Resource-Policy'), null);
  assert.deepEqual([...new Uint8Array(await response.arrayBuffer())], [1,2,3]);
});

test('rejects arbitrary origins, paths, methods and redirects', async () => {
  let calls = 0;
  const handler = createHandler(async () => { calls++; return new Response(null, { status:302, headers: { Location:'https://evil.example/file' } }); });
  for (const path of ['/tag/file?url=https://evil.example', '/tag/%2fsecret', '/tag/a..zip', '/tag/file/extra']) assert.equal((await handler(request(path))).status, 400);
  assert.equal((await handler(request(undefined, { headers:{Origin:'https://evil.example'} }))).status,403);
  assert.equal((await handler(request(undefined, {method:'POST'}))).status,405);
  assert.equal(calls,0);
  assert.equal((await handler(request())).status,502);
  assert.equal(calls,1);
});

test('preflight and HEAD work without buffering', async () => {
  const handler = createHandler(async (_url, options) => { assert.equal(options.method,'HEAD'); return new Response(null, { headers:{'Content-Length':'42'} }); });
  assert.equal((await handler(request(undefined,{method:'OPTIONS'}))).status,204);
  const head = await handler(request(undefined,{method:'HEAD'}));
  assert.equal(head.status,200); assert.equal(head.headers.get('Content-Length'),'42'); assert.equal(await head.text(),'');
});

test('range requests are passed through; upstream failures retain CORS', async () => {
  const handler = createHandler(async (_url, options) => { assert.equal(options.headers.get('Range'),'bytes=0-2'); return new Response(null,{status:404}); });
  const response = await handler(request(undefined,{headers:{Origin:origin,Range:'bytes=0-2'}}));
  assert.equal(response.status,404); assert.equal(response.headers.get('Access-Control-Allow-Origin'),origin);
});

const vendor = 'https://download.vendor.example/BIOS/board.zip';
const catalog = { builds: [
  { board: 'board-a', donor_source: { url: vendor } },
  { board: 'board-b' },
] };
function releaseThen(onVendor) {
  const calls = [];
  return { calls, handler: createHandler(async (url, options) => {
    calls.push(url);
    if (url.startsWith('https://github.com/')) return new Response(null, { status: 302, headers: { Location: 'https://release-assets.githubusercontent.com/catalog' } });
    if (url.startsWith('https://release-assets.githubusercontent.com/')) return Response.json(catalog);
    return onVendor(url, options);
  }) };
}

test('streams the donor a release catalog lists, and nothing else', async () => {
  const { calls, handler } = releaseThen(() => new Response(new Uint8Array([9, 8]), { headers: { 'Content-Length': '2' } }));
  const response = await handler(request('/donor/nightly-x/board-a'));
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('Access-Control-Allow-Origin'), origin);
  assert.deepEqual([...new Uint8Array(await response.arrayBuffer())], [9, 8]);
  assert.deepEqual(calls, [
    'https://github.com/fstart-io/fstart/releases/download/nightly-x/catalog.json',
    'https://release-assets.githubusercontent.com/catalog',
    vendor,
  ]);
  assert.equal((await handler(request('/donor/nightly-x/board-b'))).status, 404);
  assert.equal((await handler(request('/donor/nightly-x/unknown'))).status, 404);
  assert.equal((await handler(request('/donor/nightly-x/a%2Fb'))).status, 400);
  assert.equal((await handler(request('/donor/nightly-x/board-a/extra'))).status, 400);
});

test('donor redirects stay on the vendor host', async () => {
  const same = releaseThen(url => url === vendor
    ? new Response(null, { status: 302, headers: { Location: '/BIOS/mirror.zip' } })
    : new Response('ok'));
  assert.equal((await same.handler(request('/donor/t/board-a'))).status, 200);
  assert.equal(same.calls.at(-1), 'https://download.vendor.example/BIOS/mirror.zip');
  const away = releaseThen(() => new Response(null, { status: 302, headers: { Location: 'https://evil.example/x' } }));
  assert.equal((await away.handler(request('/donor/t/board-a'))).status, 502);
});
