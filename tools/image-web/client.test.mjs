import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { assetURL, bundle, catalog, releases } from './client.mjs';

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
