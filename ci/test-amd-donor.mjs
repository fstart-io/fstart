// Exercise an AMD donor-backed bundle (raw ROM or vendor-style update ZIP)
// in WASM and native assemblers. Assembly parity only; synthetic fixtures are
// inert and cannot boot.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';

export function testAmdDonor({
  assembler, build, manifest, key, temporary, native, archive, keyPath, donorPath, rawDonorPath,
}) {
  const digest = b => createHash('sha256').update(b).digest('hex');
  assert.throws(() => assembler.setDonor(Buffer.from([0x50, 0x4b, 0x03, 0x04])));

  assembler.setDonor(fs.readFileSync(donorPath));
  const settings = [];
  const choice = (manifest.plan.options?.options ?? []).find(o => !o.unavailable && o.kind.enum);
  if (build.settings && choice) {
    const label = choice.kind.enum[choice.kind.enum.length - 1].label;
    assembler.setSetting(choice.key, label);
    settings.push('--setting', `${choice.key}=${label}`);
  }
  const outputs = assembler.assemble(key);

  const out = fs.mkdtempSync(path.join(temporary, `${build.board}-${build.payload}-`));
  const args = [
    'compose-bundle', '--bundle', archive, '--out', out,
    '--signing-key', keyPath, '--donor', donorPath, ...settings,
  ];
  execFileSync(native, args, { stdio: 'inherit' });
  assert.deepEqual(fs.readdirSync(out).sort(), [...outputs.keys()].sort());
  for (const [name, bytes] of outputs) {
    assert.deepEqual(Buffer.from(bytes), fs.readFileSync(path.join(out, name)), `${build.board}: ${name}`);
  }

  const romName = `${build.board}.rom`;
  const rom = outputs.get(romName);
  assert(rom && rom.length > 0, `no assembled ${romName}`);
  const report = JSON.parse(Buffer.from(outputs.get('assembly-report.json')));
  assert.equal(report.outputs[romName].sha256, digest(Buffer.from(rom)));
  if (rawDonorPath) {
    assert.equal(report.donor_sha256, digest(fs.readFileSync(rawDonorPath)));
  }

  assembler.clear();
  assert.throws(
    () => assembler.assemble(key),
    /requires a local vendor BIOS/,
    'cleared donor must not be reused',
  );
}
