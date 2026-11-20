// Exercise the real X201 bundle's optional Intel input. The raw SPI contents
// are generated for this assembly test; no vendor firmware is needed or booted.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';

export function testIntelFlash({ assembler, outputs, key, temporary, native, archive, keyPath }) {
  const digest = b => createHash('sha256').update(b).digest('hex');
  const vendor = Buffer.alloc(8 << 20, 0x6b);
  // Legacy 8 MiB single-component descriptor. Move BIOS by one page first:
  // the same flash size and a recognizable signature are not enough.
  for (const [offset, value] of [[0x10, 0x0ff0a55a], [0x14, 0x03040003], [0x18, 6], [0x30, 4],
    [0x40, 0], [0x44, 0x07ff0501], [0x48, 0x04ef0003], [0x4c, 0x00020001], [0x50, 0x1fff]]) {
    vendor.writeUInt32LE(value, offset);
  }
  assert.throws(() => assembler.inspectIntelFlash(vendor), /bios region mismatch/);
  vendor.writeUInt32LE(0x07ff0500, 0x44);
  const regions = JSON.parse(assembler.inspectIntelFlash(vendor));
  const biosStart = 0x500000;
  assert.deepEqual(regions.find(([name]) => name === 'Bios'), ['Bios', biosStart, 0x300000]);
  const input = path.join(temporary, 'synthetic-intel.rom');
  const out = fs.mkdtempSync(path.join(temporary, 'intel-full-'));
  fs.writeFileSync(input, vendor);
  execFileSync(native, ['compose-bundle', '--bundle', archive, '--signing-key', keyPath, '--intel-flash', input, '--out', out], { stdio: 'inherit' });
  assembler.setIntelFlash(vendor);
  const withFlash = assembler.assemble(key);
  const name = 'lenovo-x201-full.rom';
  assert(!outputs.has(name));
  const full = Buffer.from(withFlash.get(name));
  assert.equal(full.length, vendor.length);
  assert.deepEqual(full.subarray(0, biosStart), vendor.subarray(0, biosStart));
  assert.deepEqual(full.subarray(biosStart), Buffer.from(outputs.get('lenovo-x201-8m.pflash')).subarray(biosStart));
  assert.deepEqual(fs.readdirSync(out).sort(), [...withFlash.keys()].sort());
  for (const [file, bytes] of withFlash) assert.deepEqual(Buffer.from(bytes), fs.readFileSync(path.join(out, file)), file);
  const report = JSON.parse(Buffer.from(withFlash.get('assembly-report.json')));
  assert.equal(report.intel_flash.sha256, digest(vendor));
  assert.equal(report.outputs[name].sha256, digest(full));
  for (const [kind, offset, size] of regions.filter(([kind]) => kind !== 'Bios')) {
    assert.deepEqual(report.intel_flash.preserved_regions.find(r => r.region === kind.toLowerCase()),
      { region: kind.toLowerCase(), offset, size, sha256: digest(vendor.subarray(offset, offset + size)) });
  }
  assembler.clear();
  assert(!assembler.assemble(key).has(name), 'removed vendor file must not be reused');
}
