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
    try {
      manifest = JSON.parse(assembler.manifest());
    } catch (error) {
      assembler.free();
      throw error;
    }
    bundles.set(token, assembler);
    return { manifest };
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
  async assemble({ token, donor, inputs, key }) {
    const bundle = opened(token);
    if (donor) bundle.setDonor(donor);
    for (const [name, bytes] of Object.entries(inputs)) bundle.setInput(name, bytes);
    const outputs = [...bundle.assemble(key ?? undefined)];
    return { result: outputs, transfer: outputs.map(([, bytes]) => bytes.buffer) };
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
