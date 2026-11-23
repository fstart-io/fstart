// Runs the WASM assembler off the UI thread. Each opened bundle is held under
// the token the page gave it; requests name the token, so a late request or a
// late open can never act on a bundle the page has moved away from. The page
// closes bundles it no longer shows.
import init, { ImageAssembler } from "./pkg/fstart_image_web.js";

const ready = init();
const bundles = new Map();

const operations = {
  async open({ bundle, token }) {
    const assembler = new ImageAssembler(bundle);
    let manifest;
    let updateRequiresDonor;
    let defaultSecurityVersion;
    try {
      manifest = JSON.parse(assembler.manifest());
      updateRequiresDonor = assembler.updateRequiresDonor();
      defaultSecurityVersion = assembler.defaultSecurityVersion() ?? null;
    } catch (error) {
      assembler.free();
      throw error;
    }
    bundles.set(token, assembler);
    return { manifest, updateRequiresDonor, defaultSecurityVersion };
  },
  // Frees the named bundles.
  async close({ tokens }) {
    for (const token of tokens) {
      bundles.get(token)?.free();
      bundles.delete(token);
    }
    return {};
  },
  async binary({ token, path }) {
    const bytes = opened(token).binary(path);
    return { result: bytes, transfer: [bytes.buffer] };
  },
  async inspectIntelFlash({ token, bytes }) {
    return { result: opened(token).inspectIntelFlash(bytes) };
  },
  async assemble({ token, donor, intelFlash, inputs, settings = {}, secureBoot = null, update = null, key }) {
    const bundle = opened(token);
    // Each request names everything the image takes: a setting set back to
    // the board's value or a removed file must not linger from the last.
    bundle.clear();
    try {
      if (donor) bundle.setDonor(donor);
      if (intelFlash) bundle.setIntelFlash(intelFlash);
      for (const [name, bytes] of Object.entries(inputs)) bundle.setInput(name, bytes);
      for (const [option, value] of Object.entries(settings)) bundle.setSetting(option, value);
      if (secureBoot) {
        const { enforce, pk, kek, db, dbx } = secureBoot;
        bundle.setSecureBoot(enforce, pk ?? undefined, kek, db, dbx);
      }
      if (update) {
        if (update.updateOnly) bundle.setUpdateOnly(true);
        if (update.fwVersion != null) bundle.setFirmwareVersion(update.fwVersion);
        if (update.securityVersion != null) bundle.setSecurityVersion(String(update.securityVersion));
        if (update.capsule) {
          const { rsaKey, cert, monotonicCount, lowestSupportedVersion } = update.capsule;
          bundle.setCapsuleSigning(
            rsaKey,
            cert,
            String(monotonicCount ?? "0"),
            lowestSupportedVersion ?? undefined,
          );
        }
      }
      const outputs = [...bundle.assemble(key ?? undefined)];
      return { result: outputs, transfer: outputs.map(([, bytes]) => bytes.buffer) };
    } finally {
      bundle.clear();
    }
  },
};

function opened(token) {
  const assembler = bundles.get(token);
  if (!assembler) throw new Error("The selected build changed; try again");
  return assembler;
}

self.onmessage = async ({ data: { id, op, args } }) => {
  try {
    await ready;
    const { result, transfer = [], ...rest } = await operations[op](args);
    self.postMessage({ id, ok: true, result: result ?? rest }, transfer);
  } catch (error) {
    self.postMessage({ id, ok: false, error: String(error?.message ?? error) });
  }
};
