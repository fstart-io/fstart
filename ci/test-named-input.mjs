// Exercise a bundle's required named input (flat kernel) in WASM and native
// assemblers. Assembly parity only; the fixture is not a bootable kernel.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';

export function testNamedInput({ assembler, build, manifest, key, temporary, native, archive, keyPath }) {
  const kernel = Buffer.from('fstart named input parity fixture\n'.repeat(128));
  const slot = manifest.inputs.find(i => i.name === 'kernel');
  assert(slot && slot.capacity >= kernel.length, 'missing kernel slot in bundle');
  assert.deepEqual(
    build.inputs.filter(i => i.name === 'kernel'),
    [slot],
    'catalog must advertise the required kernel slot',
  );
  assert.throws(() => assembler.assemble(key), /missing input kernel/);
  assert.throws(() => assembler.setInput('kernel', new Uint8Array()), /empty/);
  assert.throws(() => assembler.setInput('unknown', kernel), /unknown input/);
  assert.throws(() => assembler.setInput('kernel', Buffer.alloc(slot.capacity + 1)), /exceeds capacity/);

  const file = path.join(temporary, 'zImage');
  fs.writeFileSync(file, kernel);
  assembler.setInput('kernel', kernel);
  const outputs = assembler.assemble(key);

  const out = fs.mkdtempSync(path.join(temporary, `${build.board}-${build.payload}-`));
  execFileSync(
    native,
    ['compose-bundle', '--bundle', archive, '--out', out, '--signing-key', keyPath, '--input', `kernel=${file}`],
    { stdio: 'inherit' },
  );
  assert.deepEqual(fs.readdirSync(out).sort(), [...outputs.keys()].sort());
  for (const [name, bytes] of outputs) {
    assert.deepEqual(Buffer.from(bytes), fs.readFileSync(path.join(out, name)), `${build.board}: ${name}`);
  }

  const report = JSON.parse(Buffer.from(outputs.get('assembly-report.json')));
  assert.equal(report.input_sha256.kernel, createHash('sha256').update(kernel).digest('hex'));
  execFileSync(
    'cargo',
    [
      'run', '--locked', '--quiet', '--release', '-p', 'fstart-image-build',
      '--example', 'check-packed-kernel', '--',
      path.join(out, `${build.board}.ffs`),
      file,
    ],
    { stdio: 'inherit' },
  );

  assembler.clear();
  assert.throws(() => assembler.assemble(key), /missing input kernel/, 'cleared input must not be reused');
}
