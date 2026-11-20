// Execute real wasm-bindgen bindings in Node and compare to native assembly.
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import assert from 'node:assert/strict';
import { generateKeyPairSync } from 'node:crypto';
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import { testIntelFlash } from './test-intel-flash.mjs';
import { testNamedInput } from './test-named-input.mjs';
import { bundleFormatVersion } from '../tools/image-web/client.mjs';
const [directory, bindings, native] = process.argv.slice(2).map(p => path.resolve(p));
const { ImageAssembler } = createRequire(import.meta.url)(bindings);
const catalog = JSON.parse(fs.readFileSync(path.join(directory, 'catalog.json')));
assert.equal(catalog.format_version, bundleFormatVersion);
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'fstart-web-test-'));
const key = generateKeyPairSync('ed25519').privateKey.export({ type: 'pkcs8', format: 'der' });
const keyPath = path.join(temporary, 'key.der');
fs.writeFileSync(keyPath, key);
let comparedIntel = false;
let compared = 0;
let suppliedInput = false;
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
      // Every build that needs no file from the user is assembled both
      // ways. Vendor images remain local: the AMD builds are exercised when
      // their donor is supplied (a dual-generation SP3 donor, a
      // one-generation one, the S8040's vendor download for Siena, an Onyx
      // flash backup for Genoa).
      let donor;
      if (build.board === 'gigabyte-mj11-ec1') donor = process.env.FSTART_TEST_DONOR;
      else if (build.board.startsWith('tyan-s8030')) donor = process.env.FSTART_TEST_SP3_DONOR;
      else if (build.board === 'amd-ethanolx') donor = process.env.FSTART_TEST_ETHANOLX_DONOR;
      else if (build.board === 'tyan-s8040') donor = process.env.FSTART_TEST_S8040_DONOR;
      else if (build.board === 'amd-onyx') donor = process.env.FSTART_TEST_ONYX_DONOR;
      if (build.board === 'qemu-armv7' && build.payload === 'linux' && build.image_error == null) {
        testNamedInput({ assembler, build, manifest, key, temporary, native, archive, keyPath });
        suppliedInput = true;
        compared++;
      } else if (build.inputs.length === 0 && build.image_error == null && (!build.donor_required || donor)) {
        if (build.donor_required) assembler.setDonor(fs.readFileSync(donor));
        // A build whose options live in its image starts with one set.
        const settings = [];
        const choice = (manifest.plan.options?.options ?? []).find(o => !o.unavailable && o.kind.enum);
        if (build.settings && choice) {
          const label = choice.kind.enum[choice.kind.enum.length - 1].label;
          assembler.setSetting(choice.key, label);
          settings.push('--setting', `${choice.key}=${label}`);
        }
        const outputs = assembler.assemble(key);
        if (build.board === 'lenovo-x201' && ['uefi', 'uefi-ui'].includes(build.payload)) {
          testIntelFlash({ assembler, outputs, key, temporary, native, archive, keyPath });
          comparedIntel = true;
        }
        // Without a key, Web Crypto supplies an ephemeral one.
        if (compared === 0) assert(assembler.assemble(undefined).size > 0);
        const out = path.join(temporary, `${build.board}-${build.payload}`);
        const args = ['compose-bundle', '--bundle', archive, '--out', out, '--signing-key', keyPath];
        if (build.donor_required) args.push('--donor', donor);
        args.push(...settings);
        execFileSync(native, args, { stdio: 'inherit' });
        assert.deepEqual(fs.readdirSync(out).sort(), [...outputs.keys()].sort());
        for (const [name, bytes] of outputs) assert.deepEqual(Buffer.from(bytes), fs.readFileSync(path.join(out, name)), `${build.board}: ${name}`);
        compared++;
      }
    } finally { assembler.free(); }
  }
  assert(compared > 0, 'no native/WASM parity case was exercised');
  if (process.env.FSTART_REQUIRE_INPUT_CASE === '1') assert(suppliedInput, 'named input case did not run');
  if (process.env.FSTART_REQUIRE_INTEL_CASE === '1') assert(comparedIntel, 'X201 full-ROM case did not run');
  console.log(`Validated ${catalog.builds.length} bundles; ${compared} native/WASM parity cases passed`);
} finally { fs.rmSync(temporary, { recursive: true, force: true }); }
