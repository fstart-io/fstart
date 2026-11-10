import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { assetURL, bundle, catalog, donor, releases } from './client.mjs';

test('constructs restricted proxy asset paths', () => {
  assert.equal(assetURL('https://proxy.example', 'nightly-abc', 'board.zip').href, 'https://proxy.example/nightly-abc/board.zip');
  for (const value of ['../tag', 'a/b', 'a?b', 'a%2fb']) assert.throws(() => assetURL('https://proxy.example', value, 'file.zip'));
  assert.throws(() => assetURL('http://unsafe.example', 'tag', 'file.zip'));
});

test('discovers only published releases with catalogs, without credentials', async () => {
  const found = await releases({ fetch: async (_url, options) => {
    assert.equal(options.headers.Authorization, undefined);
    return Response.json([{tag_name:'nightly-a',assets:[{name:'catalog.json'}]}, {draft:true,assets:[{name:'catalog.json'}]}, {assets:[]}]);
  }});
  assert.equal(found.length,1); assert.equal(found[0].tag_name,'nightly-a');
});

test('verifies downloaded bundle size and digest', async () => {
  const bytes = new Uint8Array([1,2,3]);
  const build = {asset:'board.bundle.zip',size:3,sha256:createHash('sha256').update(bytes).digest('hex')};
  const options = {fetch:async () => new Response(bytes)};
  assert.deepEqual(await bundle('https://proxy.example','tag',build,options),bytes);
  await assert.rejects(bundle('https://proxy.example','tag',{...build,size:4},options),/length/);
  await assert.rejects(bundle('https://proxy.example','tag',{...build,sha256:'00'},options),/checksum/);
});

test('accepts the unified AMD catalog and rejects legacy recipes', async () => {
  const current = {format_version:2,builds:[]};
  assert.deepEqual(await catalog('https://proxy.example','tag',{fetch:async () => Response.json(current)}), current);
  await assert.rejects(catalog('https://proxy.example','tag',{fetch:async () => Response.json({format_version:1,builds:[]})}), /Unsupported/);
});

test('rejects unsupported catalogs and missing assets', async () => {
  await assert.rejects(catalog('https://proxy.example','tag',{fetch:async () => Response.json({format_version:99,builds:[]})}), /Unsupported/);
  await assert.rejects(catalog('https://proxy.example','tag',{fetch:async () => new Response(null,{status:404})}), /404/);
});

test('reports download progress and stops at the catalog size', async () => {
  const bytes = new Uint8Array([1, 2, 3, 4]);
  const build = {asset:'board.bundle.zip',size:4,sha256:createHash('sha256').update(bytes).digest('hex')};
  const stream = chunks => new Response(new ReadableStream({start(c) { for (const chunk of chunks) c.enqueue(chunk); c.close(); }}));
  const seen = [];
  const got = await bundle('https://proxy.example','tag',build,{fetch:async () => stream([bytes.slice(0,2), bytes.slice(2)]), onProgress:(n,total) => seen.push([n,total])});
  assert.deepEqual(got, bytes);
  assert.deepEqual(seen, [[2,4],[4,4]]);
  await assert.rejects(bundle('https://proxy.example','tag',{...build,size:3},{fetch:async () => stream([bytes])}), /larger than expected/);
});

test('release discovery honours a configured API and repository', async () => {
  let requested;
  await releases({ api: 'http://localhost:8080/api/', repo: 'me/fork', fetch: async url => { requested = url; return Response.json([]); } });
  assert.equal(requested, 'http://localhost:8080/api/repos/me/fork/releases?per_page=100');
});

test('downloads a vendor donor through the proxy donor route and checks it', async () => {
  const bytes = new Uint8Array([5, 6, 7]);
  const build = { board: 'gigabyte-mj11-ec1', donor_source: { size: 3, sha256: createHash('sha256').update(bytes).digest('hex') } };
  let requested;
  const got = await donor('https://proxy.example/base/', 'nightly-x', build, { fetch: async url => { requested = String(url); return new Response(bytes); } });
  assert.equal(requested, 'https://proxy.example/base/donor/nightly-x/gigabyte-mj11-ec1');
  assert.deepEqual(got, bytes);
  await assert.rejects(donor('https://proxy.example', 'nightly-x', { ...build, donor_source: { ...build.donor_source, sha256: '00' } }, { fetch: async () => new Response(bytes) }), /checksum/);
  await assert.rejects(donor('https://proxy.example', 'nightly-x', { board: 'x' }), /no vendor download/);
});
