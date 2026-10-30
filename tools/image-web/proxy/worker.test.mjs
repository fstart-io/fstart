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
