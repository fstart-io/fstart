// Execute real wasm-bindgen bindings in Node and compare to native assembly.
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import assert from 'node:assert/strict';
import { generateKeyPairSync } from 'node:crypto';
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import { bundleFormatVersion } from '../tools/image-web/client.mjs';
const [directory, bindings, native] = process.argv.slice(2).map(p => path.resolve(p));
const { ImageAssembler } = createRequire(import.meta.url)(bindings);
const catalog = JSON.parse(fs.readFileSync(path.join(directory, 'catalog.json')));
assert.equal(catalog.format_version, bundleFormatVersion);
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'fstart-web-test-'));
const key = generateKeyPairSync('ed25519').privateKey.export({ type: 'pkcs8', format: 'der' });
const keyPath = path.join(temporary, 'key.der');
fs.writeFileSync(keyPath, key);
let compared = 0;
assert.throws(() => new ImageAssembler(new Uint8Array([0, 1, 2])));
try {
  for (const build of catalog.builds) {
    const archive = path.join(directory, build.asset);
    const assembler = new ImageAssembler(fs.readFileSync(archive));
    try {
      const manifest = JSON.parse(assembler.manifest());
      assert.equal(manifest.format_version, catalog.format_version);
      assert.equal(manifest.revision, catalog.revision);
      assert.equal(manifest.board, build.board);
      assert.equal(manifest.payload, build.payload);
      for (const stage of manifest.stages) assert(assembler.binary(stage.run_path).length > 0);
      if (build.donor_required) assert.throws(() => assembler.assemble(key), /requires a local vendor BIOS/);
      // Vendor images remain local. Exercise fixed prefixes, dual banks and
      // mirrored full readouts when the corresponding donor is supplied.
      const representative = ['qemu-q35', 'orangepi-r1'].includes(build.board) && build.payload === 'halt';
      let donor;
      if (build.board === 'gigabyte-mj11-ec1') donor = process.env.FSTART_TEST_DONOR;
      else if (build.board.startsWith('tyan-s8030')) donor = process.env.FSTART_TEST_SP3_DONOR;
      else if (build.board === 'amd-ethanolx') donor = process.env.FSTART_TEST_ETHANOLX_DONOR;
      const amd = donor && ['halt', 'uefi', 'uefi-ui', 'uefi-basic'].includes(build.payload);
      if ((representative || amd) && build.inputs.length === 0) {
        if (build.donor_required && !donor) continue;
        if (build.donor_required) assembler.setDonor(fs.readFileSync(donor));
        const outputs = assembler.assemble(key);
        if (build.board === 'qemu-q35') assert(assembler.assemble(undefined).size > 0);
        const out = path.join(temporary, `${build.board}-${build.payload}`);
        const args = ['compose-bundle', '--bundle', archive, '--out', out, '--signing-key', keyPath];
        if (build.donor_required) args.push('--donor', donor);
        execFileSync(native, args, { stdio: 'inherit' });
        assert.deepEqual(fs.readdirSync(out).sort(), [...outputs.keys()].sort());
        for (const [name, bytes] of outputs) assert.deepEqual(Buffer.from(bytes), fs.readFileSync(path.join(out, name)), `${build.board}: ${name}`);
        compared++;
      }
    } finally { assembler.free(); }
  }
  assert(compared > 0, 'no native/WASM parity case was exercised');
  console.log(`Validated ${catalog.builds.length} bundles; ${compared} native/WASM parity cases passed`);
} finally { fs.rmSync(temporary, { recursive: true, force: true }); }
