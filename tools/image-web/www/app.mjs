// fstart firmware assembler. Plain modules, no build step: the Pages job
// copies this directory next to client.mjs and the wasm-bindgen output.
import { bundle as fetchBundle, catalog as fetchCatalog, donor as fetchDonor, githubApi, releases, repository, sha256 } from "./client.mjs";

const $ = id => document.getElementById(id);
const worker = new Worker(new URL("./assembler-worker.mjs", import.meta.url), { type: "module" });

const VENDORS = {
  amd: "AMD", asus: "ASUS", foxconn: "Foxconn", gigabyte: "GIGABYTE", google: "Google", ibase: "iBASE",
  intel: "Intel", lenovo: "Lenovo", qemu: "QEMU", sifive: "SiFive", sinovoip: "Sinovoip", sipeed: "Sipeed",
  tyan: "Tyan", xunlong: "Xunlong (Orange Pi)",
};
const PAYLOADS = {
  uefi: "CrabEFI, a UEFI firmware that boots operating systems from disk.",
  "uefi-ui": "CrabEFI with a setup menu.",
  "uefi-basic": "Minimal CrabEFI, without optional drivers.",
  linux: "Boots a Linux kernel you supply, straight from flash.",
  coreboot: "Hands off to a coreboot payload such as SeaBIOS.",
  halt: "Initializes the hardware, then stops. For bring-up and testing.",
};
const PAYLOAD_ORDER = ["uefi", "uefi-ui", "uefi-basic", "linux", "coreboot", "halt"];
const INPUTS = {
  kernel: { title: "Linux kernel", noun: "the Linux kernel", help: "bzImage on x86, Image on Arm and RISC-V." },
  firmware: { title: "Runtime firmware", noun: "the runtime firmware", help: "For example OpenSBI or TF-A, as the board's payload expects." },
  dtb: { title: "Device tree", noun: "the device tree", help: "The board's device tree blob." },
  fit: { title: "FIT image", noun: "the FIT image", help: "Kernel and device tree in one FIT." },
};
// The coreboot payload takes its ELF in the `kernel` slot.
const COREBOOT_PAYLOAD = { title: "coreboot payload", noun: "the coreboot payload", help: "An ELF payload such as SeaBIOS or GRUB." };
function inputLabel(name, payload = state.build?.payload) {
  if (name === "kernel" && payload === "coreboot") return COREBOOT_PAYLOAD;
  return INPUTS[name] ?? { title: name, noun: name, help: "" };
}
// Extra search words per vendor and per board prefix.
const ALIASES = {
  lenovo: "thinkpad", xunlong: "orange pi orangepi", sinovoip: "banana pi bananapi", sipeed: "lichee licheerv",
  qemu: "virtual emulator", gigabyte: "server", tyan: "server epyc", amd: "epyc",
};
// Output roles, most flashable first. `primary` marks files that are a
// complete image for the board.
const OUTPUTS = [
  { test: n => n.endsWith(".bank.rom"), text: "Boot bank only (16 MiB), for flash tools that mirror the bank themselves." },
  { test: n => n.endsWith("-bios-region.bin"), text: "BIOS region only, for programmers that write one region from a file." },
  { test: n => /\.rom$/.test(n), primary: true, text: "Complete flash image. Write it to the board's SPI flash." },
  { test: n => /-\d+[mk]\.pflash$/.test(n), primary: true, text: n => {
    const [, size, unit] = n.match(/-(\d+)([mk])\.pflash$/);
    return `Complete flash image (${size} ${unit === "m" ? "MiB" : "KiB"}). Write it to the board's flash chip.`;
  } },
  { test: n => /\.(bin|fd|img)$/.test(n), primary: true, text: "Complete flash image." },
  { test: n => n.endsWith(".ffs"), primary: true, text: (_name, all) => all.some(o => /\.(rom|pflash|bin|fd|img)$/.test(o.name) && !o.name.endsWith(".bank.rom"))
    ? "fstart firmware filesystem only, for tools that place it in flash themselves."
    : "Firmware image (fstart FFS) that the machine boots from." },
  { test: n => n === "assembly-report.json", text: "Revision and SHA-256 of every input and output. Keep it with the image." },
  { test: n => n.endsWith(".inputs.txt"), text: "List of the files that went into the image." },
];
const narrow = matchMedia("(max-width: 860px)");
const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");

const state = {
  config: null,
  releases: [],
  release: null, // {tag, published_at}; null while a local bundle is open
  catalog: null,
  local: null, // {name, build}: a bundle file the user opened
  board: null,
  build: null,
  manifest: null,
  bundleSha: null,
  fetched: null, // {bytes, source}: vendor BIOS fetched through the proxy
  own: null, // {bytes, name, size}: vendor BIOS file the user picked
  inputs: {},
  key: null,
  outputs: null, // {files, signer, donor}: one assembly's results
  busy: false,
  gen: 0, // bumps when the selected build changes; names the worker's bundle
  inputGen: 0, // bumps when any file changes
  releaseGen: 0,
  arch: "all",
  localPick: 0, // the newest bundle file the user opened
  token: 0, // the worker's open bundle
  slots: {}, // per file slot: the newest read, so an older one can't win
  urls: [],
  abort: new AbortController(),
  // Vendor download: agreements are kept per file (by SHA-256) for the session.
  vendor: { status: "idle", received: 0, error: null, agreed: new Set() },
};

// --- Worker RPC -----------------------------------------------------------

let nextId = 0;
let nextToken = 0;
const pending = new Map();
worker.onmessage = ({ data: { id, ok, result, error } }) => {
  const request = pending.get(id);
  pending.delete(id);
  if (ok) request.resolve(result);
  else request.reject(new Error(error));
};
worker.onerror = event => {
  banner(`The assembler stopped: ${event.message ?? "unknown error"}. Reload the page to continue.`);
  for (const request of pending.values()) request.reject(new Error("the assembler stopped"));
  pending.clear();
};
function call(op, args, transfer = []) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    worker.postMessage({ id, op, args }, transfer);
  });
}

// Makes `token` the page's bundle and frees the others in the worker.
function useBundle(token) {
  const old = state.token;
  state.token = token;
  if (old) closeBundles([old]);
}

function closeBundles(tokens) {
  call("close", { tokens }).catch(() => {});
}

// --- Small helpers ----------------------------------------------------------

function h(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value == null || value === false) continue;
    if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else if (key in node && !key.includes("-")) node[key] = value;
    else node.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) if (child != null && child !== false) node.append(child);
  return node;
}
const fill = (node, ...children) => node.replaceChildren(...children.flat().filter(c => c != null && c !== false));

function icon(name) {
  const paths = {
    check: "M3.5 8.5l3 3 6-7",
    alert: "M8 2.5 14 13H2zM8 6.5v3M8 11.2v.3",
    copy: "M5.5 5.5h7v7h-7zM3.5 10.5v-7h7",
    download: "M8 2.5v8M4.5 7 8 10.5 11.5 7M3 13.5h10",
    file: "M4 1.5h5l3 3v10H4zM9 1.5v3h3",
    external: "M9 2.5h4.5V7M13.5 2.5 7.5 8.5M11.5 9.5v4h-9v-9h4",
    back: "M10 3.5 5.5 8l4.5 4.5",
  };
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("class", `icon icon-${name}`);
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", paths[name]);
  svg.append(path);
  return svg;
}

function bytes(n) {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB"];
  let value = n;
  let unit = -1;
  do { value /= 1024; unit++; } while (value >= 1024 && unit < units.length - 1);
  return `${value.toFixed(value < 10 ? 1 : 0)}\u00a0${units[unit]}`;
}
const exact = n => `${n.toLocaleString("en-US")} bytes`;
const day = value => value ? new Date(value).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" }) : "";
const vendorName = vendor => VENDORS[vendor] ?? vendor;
const readFile = file => file.arrayBuffer().then(buffer => new Uint8Array(buffer));

// localStorage, where the browser allows it.
// "a", "a and b", "a, b and c".
function list(items) {
  return items.length < 3 ? items.join(" and ") : `${items.slice(0, -1).join(", ")} and ${items.at(-1)}`;
}

function storage(key, value) {
  try {
    if (value === undefined) return localStorage.getItem(key);
    localStorage.setItem(key, value);
  } catch {
    // Blocked storage only loses the remembered payload.
  }
  return null;
}

function announce(text) {
  $("live").textContent = "";
  requestAnimationFrame(() => { $("live").textContent = text; });
}

// Announces 25 % steps of a download, not every chunk.
function progressAnnouncer(what) {
  let last = -1;
  return (received, total) => {
    const step = Math.floor((received / total) * 4);
    if (step > last && step < 4) {
      last = step;
      if (step > 0) announce(`${what}: ${step * 25} percent`);
    }
  };
}

function banner(text) {
  $("banner").textContent = text ?? "";
  $("banner").hidden = !text;
}

function hash(value, label, labelled = false, wrap = false) {
  const copy = h("button", {
    type: "button", class: "copy", title: `Copy ${label}`, "aria-label": `Copy ${label}`,
    onclick: async () => {
      try {
        await navigator.clipboard.writeText(value);
        copy.classList.add("done");
        announce(`${label} copied`);
        setTimeout(() => copy.classList.remove("done"), 1200);
      } catch {
        announce("Copying is not allowed here; select the text instead");
      }
    },
  }, icon("copy"));
  return h("span", { class: `hash${wrap ? " wrap" : ""}` }, labelled ? h("span", { class: "hash-label" }, "SHA-256") : null, h("code", { title: value }, value), copy);
}

function saveLink(name, data, label = "Download", secondary = false) {
  const type = name.endsWith(".json") ? "application/json" : name.endsWith(".txt") ? "text/plain" : "application/octet-stream";
  const url = URL.createObjectURL(new Blob([data], { type }));
  state.urls.push(url);
  return h("a", { class: secondary ? "button secondary" : "button", href: url, download: name.split("/").pop(), "aria-label": `${label} ${name.split("/").pop()}` }, icon("download"), label);
}

function revokeLinks() {
  for (const url of state.urls) URL.revokeObjectURL(url);
  state.urls = [];
}

function scrollTo(node) {
  if (narrow.matches && node) node.scrollIntoView({ behavior: reduceMotion.matches ? "auto" : "smooth", block: "start" });
}

// Re-render `fn`, then give focus back to the element that had it, by id.
function keepingFocus(fn, fallback) {
  const active = document.activeElement;
  const id = active?.id;
  const label = active?.getAttribute?.("aria-label");
  fn();
  if (document.activeElement && document.activeElement !== document.body && document.activeElement.isConnected) return;
  const again = (id ? $(id) : null)
    || (label ? document.querySelector(`[aria-label="${CSS.escape(label)}"]`) : null)
    || (fallback ? $(fallback) : null);
  again?.focus({ preventScroll: true });
}

// --- Configuration and releases -----------------------------------------------

async function loadConfig() {
  let config = {};
  try {
    const response = await fetch("config.json", { cache: "no-store" });
    if (response.ok) config = await response.json();
  } catch {
    // No config: downloaded bundles still work.
  }
  const query = new URLSearchParams(location.search);
  // Relative URLs resolve against the page.
  const absolute = value => value ? new URL(value, location.href).href : value;
  return {
    proxy: absolute(query.get("proxy") ?? config.proxy ?? ""),
    api: absolute(query.get("api") ?? config.api ?? githubApi),
    repository: config.repository ?? repository,
  };
}

async function loadReleases() {
  if (!state.config.proxy) {
    renderReleaseOptions();
    sourceMessage(["This page has no release server configured. Open a .bundle.zip you downloaded from the ",
      h("a", { href: `https://github.com/${state.config.repository}/releases` }, "project's GitHub releases"), "."]);
    return;
  }
  try {
    state.releases = await releases({ api: state.config.api, repo: state.config.repository });
  } catch (error) {
    renderReleaseOptions("Releases unavailable");
    sourceMessage(`Could not list releases: ${error.message.replace(/\.$/, "")}. You can still open a downloaded bundle file.`, loadReleases);
    return;
  }
  state.releases.sort((a, b) => (b.published_at ?? "").localeCompare(a.published_at ?? ""));
  renderReleaseOptions();
  if (!state.releases.length) {
    sourceMessage("No release with firmware bundles has been published yet.");
    return;
  }
  await selectRelease(state.releases[0].tag_name);
}

function renderReleaseOptions(placeholder) {
  const select = $("release");
  const options = state.releases.map(r => h("option", { value: r.tag_name }, `${r.name || r.tag_name}${r.prerelease ? "" : " (stable)"}`));
  if (state.local) options.unshift(h("option", { value: "" }, `Local bundle: ${state.local.name}`));
  if (!options.length) options.push(h("option", { value: "" }, placeholder ?? (state.config.proxy ? "No releases yet" : "No release server")));
  fill(select, options);
  select.value = state.local ? "" : state.release?.tag ?? "";
  select.disabled = !state.releases.length && !state.local;
}

function sourceMessage(text, retry, offerFile = true) {
  $("intro-open").hidden = !offerFile;
  $("home").classList.add("empty-source");
  if (retry) {
    fill($("release-meta"), h("div", { class: "inline-error", role: "alert" }, icon("alert"),
      h("span", {}, text, " ", h("button", { type: "button", class: "quiet", onclick: retry }, "Retry"))));
  } else {
    fill($("release-meta"), text);
  }
  fill($("gallery"));
}

async function selectRelease(tag) {
  const generation = ++state.releaseGen;
  const release = state.releases.find(r => r.tag_name === tag);
  state.local = null;
  state.release = { tag, published_at: release?.published_at };
  state.catalog = null;
  state.board = null;
  clearBuild();
  showHome();
  renderReleaseOptions();
  fill($("release-meta"), "Loading the catalog…");
  fill($("gallery"));
  let catalog;
  try {
    catalog = await fetchCatalog(state.config.proxy, tag);
  } catch (error) {
    if (generation !== state.releaseGen) return;
    sourceMessage(`Could not load ${release?.name || tag}: ${error.message.replace(/\.$/, "")}.`, () => selectRelease(tag));
    return;
  }
  if (generation !== state.releaseGen) return;
  banner("");
  $("intro-open").hidden = true;
  $("home").classList.remove("empty-source");
  state.catalog = catalog;
  renderReleaseMeta();
  renderArch();
  renderGallery();
  route();
}

function releaseName() {
  const release = state.releases.find(r => r.tag_name === state.release?.tag);
  return release?.name || state.release?.tag || "the release";
}

function renderReleaseMeta() {
  const catalog = state.catalog;
  const built = catalog.created_at ?? state.release?.published_at;
  // What the gallery shows: halt builds hidden, setting variants folded in.
  const boards = boardEntries().length;
  $("filter").placeholder = `Search ${boards} boards`;
  fill($("release-meta"),
    `${boards} boards · ${allBuilds().length} builds`,
    built ? ` · built ${day(built)}` : "",
    " from ",
    h("a", { href: `https://github.com/${state.config.repository}/commit/${catalog.revision}`, target: "_blank", rel: "noopener" }, catalog.revision.slice(0, 12)),
    catalog.expires_at ? ` · kept until ${day(catalog.expires_at)}` : "");
}

// --- Board list ---------------------------------------------------------------

// `halt` only initializes the hardware; it never boots anything, so the
// page doesn't offer it (bundles still carry it for bring-up).
function allBuilds() {
  return state.local ? [state.local.build] : (state.catalog?.builds ?? []).filter(b => b.payload !== "halt");
}

// A Linux kernel, even a small one with its initramfs, rarely fits in less.
const LINUX_MIN = 1536 << 10;

// Why a build can't give the user an image, or null.
function unusable(b) {
  if (b.image_error) return "doesn't fit the flash";
  const kernel = b.payload === "linux" && (b.inputs ?? []).find(i => i.name === "kernel");
  if (kernel && kernel.capacity < LINUX_MIN) return `leaves only ${bytes(kernel.capacity)} for a Linux kernel`;
  return null;
}

// The board a build is shown under: variants that are a setting of another
// board (Tyan's PCIe bifurcation) appear as that board with an option.
const groupOf = b => b.option_of?.board ?? b.board;

function payloadLabel(b) {
  return b.bundled_payload ? "SeaBIOS" : b.payload;
}

const ARCHES = { x86: "x86", x86_64: "x86-64", aarch64: "AArch64", armv7: "Armv7", riscv64: "RISC-V" };
const ARCH_GROUPS = [["all", "All"], ["x86", "x86"], ["arm", "Arm"], ["riscv", "RISC-V"]];
const archGroup = arch => arch?.startsWith("x86") ? "x86" : arch?.startsWith("riscv") ? "riscv" : arch ? "arm" : "";

function boardEntries() {
  const boards = new Map();
  for (const build of allBuilds()) {
    const id = groupOf(build);
    if (!boards.has(id)) boards.set(id, { board: id, builds: [] });
    const entry = boards.get(id);
    entry.builds.push(build);
    // The base board names the group.
    if (!entry.vendor || !build.option_of) Object.assign(entry, { vendor: build.vendor ?? "", name: build.name, arch: build.arch });
  }
  return [...boards.values()];
}

function groupBuilds(board) {
  const builds = allBuilds();
  const group = groupOf(builds.find(b => b.board === board) ?? { board });
  return builds.filter(b => groupOf(b) === group);
}

function boardsByVendor(entries) {
  const vendors = new Map();
  for (const entry of entries) {
    if (!vendors.has(entry.vendor)) vendors.set(entry.vendor, []);
    vendors.get(entry.vendor).push(entry);
  }
  return [...vendors].sort(([a], [b]) => vendorName(a).localeCompare(vendorName(b)));
}

function renderArch() {
  const present = new Set(boardEntries().map(e => archGroup(e.arch)));
  const groups = ARCH_GROUPS.filter(([key]) => key === "all" || present.has(key));
  fill($("arch"), groups.length > 2 ? groups.map(([key, label]) => h("button", {
    type: "button", role: "radio", class: "seg", "aria-checked": String(state.arch === key),
    onclick: () => { state.arch = key; renderArch(); renderGallery(); },
  }, label)) : null);
}

function matchesQuery(entry, query) {
  const words = [entry.board, entry.name ?? "", entry.vendor, vendorName(entry.vendor), ALIASES[entry.vendor] ?? "",
    ARCHES[entry.arch] ?? "", entry.arch ?? "", ...entry.builds.map(b => b.payload)].join(" ").toLowerCase();
  return query.split(/\s+/).every(term => words.includes(term));
}

function renderGallery() {
  const query = $("filter").value.trim().toLowerCase();
  const entries = boardEntries()
    .filter(e => state.arch === "all" || archGroup(e.arch) === state.arch)
    .filter(e => !query || matchesQuery(e, query));
  // One grid, vendor by vendor; most vendors have a single board, so
  // per-vendor sections would be mostly empty rows.
  // Boards where nothing fits yet go last.
  const fits = e => e.builds.some(b => !unusable(b));
  const sorted = boardsByVendor(entries).flatMap(([, list]) =>
    list.sort((a, b) => (a.name ?? a.board).localeCompare(b.name ?? b.board)))
    .sort((a, b) => Number(fits(b)) - Number(fits(a)));
  const groups = sorted.length ? [h("h2", { class: "sr-only" }, "Boards"),
    h("ul", { class: "cards" }, sorted.map(entry => h("li", {}, boardCard(entry))))] : [];
  if (!entries.length && allBuilds().length) {
    groups.push(h("p", { class: "empty" }, `No board matches “${query}”${state.arch === "all" ? "" : " on this architecture"}. Try a vendor or family (thinkpad, qemu, orange pi, gigabyte) or part of a board name.`));
  }
  keepingFocus(() => fill($("gallery"), groups));
  if (query) announceCount(entries.length);
}

let countTimer;
function announceCount(shown) {
  clearTimeout(countTimer);
  countTimer = setTimeout(() => announce(`${shown} ${shown === 1 ? "board" : "boards"} found`), 400);
}

function boardCard(entry) {
  const donor = entry.builds.some(b => b.donor_required);
  const byPayload = new Map();
  for (const b of entry.builds) if (!byPayload.has(b.payload) || !unusable(b)) byPayload.set(b.payload, b);
  const payloads = [...byPayload.values()].sort((a, b) => PAYLOAD_ORDER.indexOf(a.payload) - PAYLOAD_ORDER.indexOf(b.payload));
  const usable = payloads.filter(b => !unusable(b));
  const broken = payloads.length - usable.length;
  return h("a", { class: `card${usable.length ? "" : " dim"}`, id: `card-${entry.board}`, href: `#/board/${entry.board}` },
    h("span", { class: "card-vendor" }, entry.vendor ? vendorName(entry.vendor) : "Local bundle"),
    h("span", { class: "card-name" }, entry.name ?? entry.board),
    h("span", { class: "card-id" }, entry.board),
    h("span", { class: "card-payloads" }, usable.map(b => h("span", {}, payloadLabel(b))),
      broken && usable.length ? h("span", { class: "off" }, `+${broken} too large`) : null),
    h("span", { class: "card-tags" },
      entry.arch ? h("span", { class: "tag" }, ARCHES[entry.arch] ?? entry.arch) : null,
      donor ? h("span", { class: "tag accent" }, "needs vendor BIOS") : null,
      usable.length ? null : h("span", { class: "tag warn" }, "nothing fits its flash yet")));
}

function defaultBuild(builds) {
  const usable = builds.filter(b => !unusable(b));
  const remembered = storage("fstart.payload");
  const pick = list => list.find(b => b.payload === remembered)
    ?? PAYLOAD_ORDER.map(p => list.find(b => b.payload === p)).find(Boolean)
    ?? list[0];
  return pick(usable) ?? pick(builds);
}

// True if the user still wants to leave an assembled image they haven't saved.
function leaveUnsaved() {
  if (!state.outputs || state.outputs.saved) return true;
  return confirm(`Discard the assembled ${state.build.board} image? You haven't downloaded it yet.`);
}

// Routes: "#/" is the board gallery, "#/board/<id>[/<payload>]" a board.
function boardHash(board, payload) {
  return `#/board/${board}${payload ? `/${payload}` : ""}`;
}

function chooseBoard(board, payload) {
  location.hash = boardHash(board, payload);
}

let lastHash = location.hash;
function route() {
  const [, kind, board, payload] = location.hash.match(/^#\/(board)\/([^/]+)(?:\/([^/]+))?$/) ?? [];
  const known = kind && allBuilds().some(b => b.board === board);
  const leaving = state.board && (board !== state.board || (payload && payload !== state.build?.payload));
  if (leaving && !leaveUnsaved()) {
    history.replaceState(null, "", lastHash);
    return;
  }
  lastHash = location.hash;
  if (!known) {
    if (state.board) {
      const previous = state.board;
      state.board = null;
      clearBuild();
      showHome();
      $(`card-${previous}`)?.focus({ preventScroll: false });
    } else {
      showHome();
    }
    return;
  }
  showDetail();
  if (state.board !== board || (payload && state.build?.payload !== payload)) {
    state.board = board;
    const builds = allBuilds().filter(b => b.board === board);
    selectBuild(builds.find(b => b.payload === payload) ?? defaultBuild(builds));
  }
  window.scrollTo({ top: 0, behavior: "auto" });
  $("board-name")?.focus({ preventScroll: true });
}

function showHome() {
  $("home").hidden = false;
  $("detail").hidden = true;
  fill($("build"));
  document.title = "fstart firmware assembler";
}

function showDetail() {
  $("home").hidden = true;
  $("detail").hidden = false;
}

// --- Build selection, bundle download ------------------------------------------------

// Drops the selected build. `next`, when given, is the build replacing it:
// the user's files stay when they still apply (same board for the vendor
// BIOS, a slot of the same name and enough room for inputs; the signing key
// always).
function clearBuild(next) {
  state.gen++;
  state.inputGen++;
  state.abort?.abort();
  state.abort = new AbortController();
  revokeLinks();
  const same = next && state.build && next.board === state.build.board;
  const sameDonor = same && next.donor_source && next.donor_source.sha256 === state.build.donor_source?.sha256;
  if (state.vendor.status === "fetching" && !sameDonor) {
    state.vendor.abort?.abort();
    announce("Vendor download cancelled");
  }
  const keep = {
    fetched: same && next.donor_source?.sha256 === state.fetched?.source.sha256 ? state.fetched : null,
    own: same && next.donor_required ? state.own : null,
    inputs: next ? Object.fromEntries(Object.entries(state.inputs)
      .filter(([name, file]) => (next.inputs ?? []).some(i => i.name === name && i.capacity >= file.size)
        && inputLabel(name, state.build?.payload) === inputLabel(name, next.payload))) : {},
  };
  Object.assign(state, { build: null, manifest: null, bundleSha: null, outputs: null, busy: false, ...keep });
  if (!(sameDonor && state.vendor.status === "fetching")) {
    state.vendor = { ...state.vendor, status: "idle", received: 0, error: null, abort: null, expanded: false };
  }
}

// A fetch that the next build change aborts.
function abortable() {
  const signal = state.abort.signal;
  return (url, init = {}) => fetch(url, { ...init, signal });
}


async function selectBuild(build, { keepFocus = false } = {}) {
  clearBuild(build);
  const generation = state.gen;
  state.build = build;
  if (keepFocus) keepingFocus(renderBuild);
  else renderBuild();
  try {
    const progress = progressAnnouncer("Bundle download");
    const data = await fetchBundle(state.config.proxy, state.release.tag, build, {
      fetch: abortable(),
      onProgress: (received, total) => {
        if (generation !== state.gen) return;
        setBundleProgress(received, total);
        progress(received, total);
      },
    });
    if (generation !== state.gen) return;
    const token = ++nextToken;
    const { manifest } = await call("open", { bundle: data, token }, [data.buffer]);
    if (generation !== state.gen) {
      closeBundles([token]);
      return;
    }
    useBundle(token);
    if (build.board !== manifest.board || build.payload !== manifest.payload) throw new Error("the bundle does not match the catalog entry");
    state.bundleSha = build.sha256;
    state.manifest = manifest;
    keepingFocus(renderBuild);
    announce(`${manifest.board}, ${manifest.payload}: bundle checked and ready`);
  } catch (error) {
    if (generation !== state.gen) return;
    fill($("bundle-state"), h("div", { class: "inline-error", role: "alert" }, icon("alert"),
      h("span", {}, `Could not get the bundle: ${error.message}.`),
      h("button", { type: "button", class: "quiet", id: "bundle-retry", onclick: () => { selectBuild(build); $("board-name")?.focus(); } }, "Retry")));
  }
}

// Opening a file replaces the current build only once the file has turned
// out to be a bundle; otherwise the page stays as it was, with an error.
async function openLocalBundle(file) {
  const generation = state.gen;
  const releaseGeneration = state.releaseGen;
  const pick = ++state.localPick;
  const stale = () => generation !== state.gen || releaseGeneration !== state.releaseGen || pick !== state.localPick;
  banner("");
  announce(`Opening ${file.name}`);
  const previousMeta = [...$("release-meta").childNodes];
  fill($("release-meta"), `Opening ${file.name}…`);
  const restoreMeta = () => { if ($("release-meta").textContent === `Opening ${file.name}…`) fill($("release-meta"), previousMeta); };
  let data;
  let digest;
  try {
    data = await readFile(file);
    digest = await sha256(data);
  } catch (error) {
    restoreMeta();
    if (!stale()) banner(`Could not read ${file.name}: ${error.message}`);
    return;
  }
  if (stale()) {
    restoreMeta();
    return;
  }
  const token = ++nextToken;
  let manifest;
  try {
    ({ manifest } = await call("open", { bundle: data, token }, [data.buffer]));
  } catch (error) {
    restoreMeta();
    if (stale()) return;
    banner(`${file.name} is not a bundle this page can use: ${error.message}`);
    return;
  }
  if (stale() || !leaveUnsaved()) {
    restoreMeta();
    closeBundles([token]);
    return;
  }
  ++state.releaseGen;
  clearBuild();
  useBundle(token);
  const build = {
    board: manifest.board, vendor: "", payload: manifest.payload, asset: file.name, size: file.size,
    inputs: manifest.inputs, donor_required: "Amd" in manifest.assembly, binaries: Object.keys(manifest.files), local: true,
  };
  state.release = null;
  state.catalog = null;
  state.local = { name: file.name, build };
  state.board = build.board;
  state.build = build;
  state.bundleSha = digest;
  state.manifest = manifest;
  $("filter").value = "";
  $("filter").placeholder = "Search boards";
  renderReleaseOptions();
  fill($("release-meta"), `Opened from ${file.name}, revision ${manifest.revision.slice(0, 12)}.`,
    state.config.proxy && state.releases.length ? " Pick a release above to go back." : "");
  renderArch();
  renderGallery();
  lastHash = boardHash(build.board);
  history.pushState(null, "", lastHash);
  showDetail();
  renderBuild();
  announce(`${manifest.board}, ${manifest.payload}: opened from ${file.name}`);
  $("board-name")?.focus({ preventScroll: true });
}

// --- Detail panel ----------------------------------------------------------------

function renderBuild() {
  const build = state.build;
  const root = $("build");
  document.title = `${build.name ?? build.board} · fstart firmware assembler`;
  const ready = state.manifest && !unusable(build);
  fill(root,
    header(build),
    spec(build),
    h("div", { id: "bundle-state" }, bundleState()),
    unusable(build) ? imageError(build) : null,
    ready ? needs(build) : null,
    ready ? assembleBlock() : null,
    state.outputs ? outputs() : null,
    state.manifest ? binaries(build) : null);
  updateAssemble();
}

function header(build) {
  const siblings = allBuilds().filter(b => b.board === build.board)
    .sort((a, b) => PAYLOAD_ORDER.indexOf(a.payload) - PAYLOAD_ORDER.indexOf(b.payload));
  const group = groupBuilds(build.board);
  const base = group.find(b => !b.option_of) ?? build;
  const variants = [...new Map(group.map(b => [b.board, b])).values()];
  return h("header", { class: "build-head" },
    h("p", { class: "eyebrow" }, build.local ? "Local bundle" : [vendorName(build.vendor), build.arch ? ` · ${ARCHES[build.arch] ?? build.arch}` : ""].join("")),
    h("h1", { class: "board-name", id: "board-name", tabindex: "-1" }, base.name ?? build.board),
    h("p", { class: "board-id" }, build.board),
    variants.length > 1 ? h("fieldset", { class: "payloads options" },
      h("legend", {}, "Configuration"),
      h("div", { class: "chips" }, variants.map(v => h("label", { class: "chip sans" },
        h("input", {
          type: "radio", name: "variant", id: `variant-${v.board}`, value: v.board, checked: v.board === build.board,
          disabled: state.busy, onchange: () => chooseBoard(v.board, allBuilds().some(b => b.board === v.board && b.payload === build.payload) ? build.payload : undefined),
        }),
        h("span", {}, v.option_of?.option ?? "Standard"))))) : null,
    h("fieldset", { class: "payloads" },
      h("legend", {}, "Payload"),
      h("div", { class: "chips" }, siblings.map(b => h("label", { class: `chip${unusable(b) ? " broken" : ""}` },
        h("input", {
          type: "radio", name: "payload", id: `payload-${b.payload}`, value: b.payload, checked: b === build,
          disabled: state.busy, onchange: () => {
            if (!leaveUnsaved()) {
              keepingFocus(renderBuild);
              return;
            }
            storage("fstart.payload", b.payload);
            lastHash = boardHash(b.board, b.payload);
            history.replaceState(null, "", lastHash);
            selectBuild(b, { keepFocus: true });
          },
        }),
        h("span", {}, payloadLabel(b)),
        unusable(b) ? h("span", { class: "sr-only" }, `, ${unusable(b)}`) : null)))),
    h("p", { class: "payload-desc" }, payloadDescription(build),
      unusable(build) ? h("b", { class: "warn-text" }, ` This build ${unusable(build)}.`) : null));
}

function payloadDescription(build) {
  if (build.bundled_payload) {
    return `${build.bundled_payload}, a legacy PC BIOS, run as a coreboot payload. CI built it; you can use your own coreboot payload instead.`;
  }
  return PAYLOADS[build.payload] ?? "";
}

function spec(build) {
  const rows = [
    ["Bundle", h("span", {}, h("code", {}, build.asset), ` · ${bytes(build.size)}`)],
    ["SHA-256", h("span", { class: "stack" }, hash(state.bundleSha ?? build.sha256, "bundle SHA-256", false, true),
      !build.local && state.manifest ? h("span", { class: "checked" }, icon("check"), `Matches catalog.json of ${releaseName()}`) : null)],
  ];
  const revision = state.manifest?.revision ?? state.catalog?.revision;
  if (revision) {
    const when = build.local ? null : state.catalog?.created_at ?? state.release?.published_at;
    rows.push(["Source", h("span", {},
      h("a", { href: `https://github.com/${state.config.repository}/commit/${revision}`, target: "_blank", rel: "noopener" }, revision.slice(0, 12)),
      when ? ` · ${day(when)}` : "")]);
  }
  if (build.local) rows.push(["Checked", "This is your file's hash; it isn't compared against a release catalog."]);
  return h("dl", { class: "spec" }, rows.map(([term, value]) => h("div", {}, h("dt", {}, term), h("dd", {}, value))));
}

function bundleState() {
  if (state.manifest) return null;
  return h("div", { class: "progress-row" },
    h("progress", { id: "bundle-progress", max: state.build.size, value: 0, "aria-label": "Bundle download" }),
    h("span", { id: "bundle-progress-text" }, "Downloading the bundle…"));
}

function setBundleProgress(received, total) {
  const bar = $("bundle-progress");
  if (!bar) return;
  bar.value = received;
  $("bundle-progress-text").textContent = `Downloading the bundle · ${bytes(received)} of ${bytes(total)}`;
}

function imageError(build) {
  const kernel = !build.image_error && build.inputs.find(i => i.name === "kernel");
  return h("div", { class: "notice warn" },
    icon("alert"),
    h("div", {},
      kernel
        ? h("p", {}, h("b", {}, "A Linux kernel won't fit this board's flash."), ` The image leaves ${bytes(kernel.capacity)} for it; even a small kernel with its initramfs needs more.`)
        : [h("p", {}, h("b", {}, "This build does not fit the board's flash"), ", so no image can be assembled from it."),
          h("p", { class: "detail-text" }, build.image_error)],
      h("p", {}, "Pick another payload, or download its compiled binaries below.")));
}

// --- Your files ----------------------------------------------------------------

function needs(build) {
  const rows = [];
  if (build.donor_required) rows.push(donorRow(build));
  for (const input of state.manifest.inputs) rows.push(inputRow(input));
  rows.push(keyRow());
  return h("section", { class: "needs", "aria-labelledby": "needs-title" },
    h("h2", { id: "needs-title" }, "Your files"),
    build.donor_required || requiredInputs().length ? null : h("p", { class: "muted" }, "Nothing is required. You can add your signing key."),
    rows);
}

function row({ title, help, status, body }) {
  return h("div", { class: "need" },
    h("div", { class: "need-head" },
      h("h3", {}, title),
      h("span", { class: `status ${status.kind ?? (status.ok ? "ok" : "needed")}` }, status.ok ? icon("check") : null, status.text)),
    help ? h("p", { class: "muted" }, help) : null,
    body);
}

// A file slot. Its buttons have stable ids, so re-rendering keeps focus.
// It reads the file itself and hands over the bytes only if the user is
// still on the same build (unless `anyBuild`) and picked nothing newer for
// this slot meanwhile. `check` can reject a file before it is read.
function filePicker({ id, label, slot, accept, current, onFile, onClear, check, anyBuild = false }) {
  const take = async file => {
    const problem = check?.(file);
    if (problem) return problem;
    const generation = state.gen;
    const seq = (state.slots[id] ?? 0) + 1;
    state.slots[id] = seq;
    const data = await readFile(file);
    if ((!anyBuild && generation !== state.gen) || state.slots[id] !== seq) return null;
    onFile(file, data);
    return null;
  };
  const input = h("input", { type: "file", id: `${id}-input`, accept: accept ?? "", class: "sr-only", tabindex: "-1" });
  input.addEventListener("change", async () => {
    const file = input.files[0];
    input.value = "";
    if (file) await take(file);
  });
  const zone = h("div", { class: `drop${current ? " filled" : ""}` },
    icon("file"),
    current
      ? h("span", { class: "drop-file" }, h("b", {}, current.name), `\u00a0· ${bytes(current.size)}`)
      : h("span", { class: "drop-hint" }, "Drop a file here or"),
    h("button", {
      type: "button", class: "quiet", id: `${id}-pick`, disabled: state.busy, onclick: () => input.click(),
      "aria-label": current ? `Replace ${slot} (${current.name})` : `${label}…`,
    }, current ? "Replace…" : `${label}…`),
    current && onClear ? h("button", {
      type: "button", class: "quiet", id: `${id}-clear`, disabled: state.busy, "aria-label": `Remove ${slot} (${current.name})`,
      onclick: () => { state.slots[id] = (state.slots[id] ?? 0) + 1; onClear(`${id}-pick`); },
    }, "Remove") : null,
    input);
  zone.addEventListener("dragover", event => { event.preventDefault(); zone.classList.add("over"); });
  zone.addEventListener("dragleave", () => zone.classList.remove("over"));
  zone.addEventListener("drop", async event => {
    event.preventDefault();
    zone.classList.remove("over");
    const file = event.dataTransfer.files[0];
    if (file && !state.busy) await take(file);
  });
  return zone;
}

// Any file change invalidates earlier outputs and in-flight assembly.
// Outputs stay downloadable but are marked out of date: changing a file
// must not silently delete an image the user hasn't saved.
function filesChanged(message, fallbackFocus) {
  state.inputGen++;
  if (state.outputs) state.outputs.outdated = true;
  keepingFocus(renderBuild, fallbackFocus);
  if (message) announce(message);
}

function donorRow(build) {
  const source = build.donor_source;
  // Your own file is only known to fit once an assembly succeeds.
  const status = state.own ? { kind: "neutral", text: `Your file ${state.own.name} · checked when you assemble` }
    : state.fetched ? { ok: true, text: `Using ${state.fetched.source.name}` }
    : { text: "Needed" };
  const offerVendor = source && !build.local;
  return row({
    title: "Vendor BIOS",
    help: "AMD's PSP firmware and the CPU microcode can't be redistributed, so the image takes them from the vendor's BIOS on your machine. A vendor ZIP works as it is.",
    status,
    body: h("div", { class: "donor" },
      offerVendor ? vendorFetch(source) : null,
      h("div", { class: "own" },
        offerVendor ? h("p", { class: "or" }, state.own ? "Your own file, used instead of the vendor download:" : "Or use your own file, such as a backup of this board's flash:") : null,
        filePicker({
          id: "donor", label: "Choose a BIOS file", slot: "the vendor BIOS", accept: ".zip,.rom,.bin,.fd,.img", current: state.own,
          onFile: (file, data) => {
            state.own = { name: file.name, size: file.size, bytes: data };
            filesChanged(`Using ${file.name} as the vendor BIOS`);
          },
          onClear: fallback => {
            state.own = null;
            filesChanged(state.fetched ? `Using ${state.fetched.source.name}` : "Vendor BIOS removed", fallback);
          },
        }))),
  });
}

// The vendor download is state, not DOM: re-rendering the panel (adding a
// key, removing a file) keeps the user's agreement and a running download.
function vendorFetch(source) {
  const note = source.note && !state.own ? h("p", { class: "vendor-note" }, icon("alert"), h("span", {}, source.note)) : null;
  const meta = h("div", { class: "vendor-meta" },
    h("span", {}, `${bytes(source.size)} · `, h("a", { href: source.page, target: "_blank", rel: "noopener" }, "vendor download page", icon("external"))),
    hash(source.sha256, `${source.name} SHA-256`, true, true));
  if (state.fetched) {
    return h("div", { class: `vendor-box done${state.own ? " unused" : ""}`, id: "vendor-done", tabindex: "-1" },
      h("p", { class: "vendor-title" }, icon("check"), h("span", {}, h("b", {}, source.name), ` from ${source.vendor}, downloaded; SHA-256 matches.`)),
      state.own ? h("p", { class: "unused-note" }, "Not used: your own file below takes precedence.") : null,
      meta, note);
  }
  const fetching = state.vendor.status === "fetching";
  if (state.own && !fetching && !state.vendor.expanded) {
    return h("p", { class: "vendor-collapsed" },
      h("button", { type: "button", class: "quiet", id: "vendor-expand", onclick: () => { state.vendor.expanded = true; keepingFocus(renderBuild, "donor-terms"); $("donor-terms")?.focus(); } },
        `Or download ${source.name} from ${source.vendor} instead`));
  }
  const agreed = state.vendor.agreed.has(source.sha256);
  const agree = h("input", { type: "checkbox", id: "donor-terms", "aria-describedby": "donor-how", checked: agreed, disabled: fetching || state.busy });
  agree.addEventListener("change", () => {
    if (agree.checked) state.vendor.agreed.add(source.sha256);
    else state.vendor.agreed.delete(source.sha256);
    $("donor-fetch").disabled = !agree.checked || state.busy;
  });
  const button = fetching
    ? h("button", { type: "button", id: "donor-fetch", onclick: cancelVendor }, h("span", {}, "Cancel download"))
    : h("button", {
      type: "button", class: "primary", id: "donor-fetch", disabled: !agreed || state.busy,
      onclick: () => fetchVendor(source),
    }, icon("download"), h("span", {}, state.vendor.status === "error" ? "Retry vendor download" : `Download from ${source.vendor} · ${bytes(source.size)}`));
  return h("div", { class: "vendor-box" },
    h("p", { class: "vendor-title" }, h("b", {}, source.name), ` · ${source.vendor}`),
    meta, note,
    h("label", { class: "terms" }, agree,
      h("span", {}, `I agree to ${source.vendor}'s `,
        h("a", { href: source.terms, target: "_blank", rel: "noopener" }, "terms of use", icon("external")),
        ` for downloading ${source.name}.`)),
    h("p", { class: "vendor-how", id: "donor-how" }, `The file comes straight from ${source.vendor} through the fstart proxy, is checked against the SHA-256 above, and stays in this tab.`),
    h("div", { class: "actions" }, button),
    fetching ? h("div", { class: "progress-row" },
      h("progress", { id: "vendor-progress", max: source.size, value: state.vendor.received, "aria-label": `${source.name} download` }),
      h("span", { id: "vendor-progress-text" }, state.vendor.received ? `${bytes(state.vendor.received)} of ${bytes(source.size)}` : `Connecting to ${source.vendor}…`)) : null,
    state.vendor.status === "error" ? h("div", { class: "inline-error", role: "alert" }, icon("alert"),
      h("span", {}, `${state.vendor.error}. Retry, or use your own file below.`)) : null);
}

async function fetchVendor(source) {
  // Keyed to the board and file, not the build: switching payload keeps it.
  const board = state.board;
  const ours = () => state.board === board && state.build?.donor_source?.sha256 === source.sha256 && state.vendor.abort === controller;
  const controller = new AbortController();
  state.vendor = { ...state.vendor, status: "fetching", received: 0, error: null, abort: controller };
  keepingFocus(renderBuild);
  $("donor-fetch")?.focus();
  const announceProgress = progressAnnouncer(`${source.name} download`);
  try {
    const data = await fetchDonor(state.config.proxy, state.release.tag, state.build, {
      fetch: (url, init = {}) => fetch(url, { ...init, signal: controller.signal }),
      onProgress: (received, total) => {
        if (!ours()) return;
        state.vendor.received = received;
        const bar = $("vendor-progress");
        if (bar) bar.value = received;
        const text = $("vendor-progress-text");
        if (text) text.textContent = `${bytes(received)} of ${bytes(total)}`;
        announceProgress(received, total);
      },
    });
    if (!ours()) return;
    state.vendor = { ...state.vendor, status: "idle", abort: null };
    state.fetched = { bytes: data, source };
    if (state.own) {
      // Your own file still wins: outputs and any assembly stay valid.
      keepingFocus(renderBuild);
      announce(`${source.name} downloaded and checked; your own file is still used`);
      return;
    }
    const from = document.activeElement;
    filesChanged(`${source.name} downloaded and checked`);
    if (!from || from === document.body || from.id === "donor-fetch" || !from.isConnected) focusNext();
  } catch (err) {
    if (!ours()) return;
    const cancelled = controller.signal.aborted;
    state.vendor = { ...state.vendor, status: cancelled ? "idle" : "error", error: cancelled ? null : err.message.replace(/\.$/, ""), abort: null };
    keepingFocus(renderBuild);
    $("donor-fetch")?.focus();
    announce(cancelled ? "Vendor download cancelled" : `Vendor download failed: ${err.message}`);
  }
}

function cancelVendor() {
  state.vendor.abort?.abort();
}

// After a file arrives, go to whatever is still missing, else to Assemble.
function focusNext() {
  const missingInput = requiredInputs().find(i => !state.inputs[i.name]);
  const target = missingInput ? $(`input-${missingInput.name}-pick`) : $("assemble");
  (target && !target.disabled ? target : $("vendor-done") ?? $("detail"))?.focus({ preventScroll: true });
}

// Inputs the user must supply: a payload the bundle carries is optional.
function requiredInputs() {
  const bundled = state.manifest?.bundled_payload?.input;
  return (state.manifest?.inputs ?? []).filter(i => i.name !== bundled);
}

function inputRow(input) {
  const bundled = state.manifest.bundled_payload?.input === input.name ? state.manifest.bundled_payload : null;
  const { title: label, help: labelHelp, noun } = inputLabel(input.name);
  const title = bundled ? `${label} (optional)` : label;
  const help = bundled ? `${bundled.name} is included. Add your own ELF payload to use it instead.` : labelHelp;
  const current = state.inputs[input.name];
  const error = h("div", { id: `input-${input.name}-error` });
  return row({
    title,
    help: h("span", {}, help ? `${help} ` : "",
      input.default ? ["CI's reference build used ", h("code", {}, input.default.split("/").pop()), ". "] : null,
      `At most ${bytes(input.capacity)}.`),
    status: current ? { ok: true, text: "Added" } : bundled ? { ok: true, text: `Using ${bundled.name}` } : { text: "Needed" },
    body: h("div", {}, filePicker({
      id: `input-${input.name}`, label: `Choose ${noun}`, slot: noun, current,
      check: file => {
        if (file.size <= input.capacity) return null;
        fill(error, h("p", { class: "inline-error", role: "alert" }, icon("alert"),
          `${file.name} is ${bytes(file.size)}; this slot holds at most ${bytes(input.capacity)}.`));
        return "too large";
      },
      onFile: (file, data) => {
        state.inputs[input.name] = { name: file.name, size: file.size, bytes: data };
        filesChanged(`${file.name} added`);
      },
      onClear: fallback => { delete state.inputs[input.name]; filesChanged(`${title} removed`, fallback); },
    }), error),
  });
}

function keyRow() {
  return row({
    title: "Signing key (optional)",
    help: h("span", {}, "The image is signed with Ed25519. Without your key, a throwaway key signs it, and later updates can't be signed to match. Use your PKCS#8 key (",
      h("code", {}, "openssl genpkey -algorithm ed25519 -out key.pem"), "), or create one here."),
    status: state.key ? { ok: true, text: `Your key ${state.key.name}` } : { kind: "neutral", text: "Throwaway key" },
    body: h("div", { class: "key-body" },
      filePicker({
        id: "key", label: "Choose a key", slot: "the signing key", accept: ".pem,.der,.key", current: state.key,
        anyBuild: true,
        onFile: (file, data) => { state.key = { name: file.name, size: file.size, bytes: data }; filesChanged(`Signing with ${file.name}`); },
        onClear: fallback => { state.key = null; filesChanged("Signing with a throwaway key", fallback); },
      }),
      state.key?.created
        ? h("p", { class: "key-created" }, icon("alert"), h("span", {},
          "This key was created in this tab and exists only in the file you saved. Keep it safe: you need it to sign updates for this board. ",
          saveLink(state.key.name, state.key.bytes, "Save it again", true)))
        : state.key ? null : h("button", { type: "button", class: "quiet", id: "key-create", onclick: createKey }, "Create a new key")),
  });
}

// Ed25519 in Web Crypto; the private key leaves the tab only as the file
// the user saves.
async function createKey() {
  let pem;
  try {
    const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
    const der = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
    const base64 = btoa(String.fromCharCode(...der)).replace(/.{64}/g, "$&\n");
    pem = `-----BEGIN PRIVATE KEY-----\n${base64}\n-----END PRIVATE KEY-----\n`;
  } catch {
    announce("This browser can't create Ed25519 keys; use openssl instead");
    banner("This browser can't create Ed25519 keys. Create one with openssl genpkey -algorithm ed25519 -out key.pem and choose it.");
    return;
  }
  const bytes = new TextEncoder().encode(pem);
  const name = `fstart-${state.build?.board ?? "signing"}-key.pem`;
  state.key = { name, size: bytes.length, bytes, created: true };
  filesChanged(`Created ${name}; save it to keep signing updates`);
  // Offer the file right away: it's the only copy.
  const link = saveLink(name, bytes);
  link.click();
  $("key-pick")?.focus();
}

// --- Assemble and outputs --------------------------------------------------------

function missing() {
  const list = [];
  if (state.build.donor_required && !state.own && !state.fetched) list.push("the vendor BIOS");
  for (const input of requiredInputs()) if (!state.inputs[input.name]) list.push(inputLabel(input.name).noun);
  return list;
}

function assembleBlock() {
  return h("section", { class: "assemble", "aria-labelledby": "assemble-title" },
    h("h2", { id: "assemble-title", class: "sr-only" }, "Assemble"),
    h("div", { class: "actions" },
      h("button", { type: "button", id: "assemble", class: state.outputs ? "big" : "primary big", onclick: assemble }, state.outputs ? "Assemble again" : "Assemble image"),
      h("p", { id: "assemble-status", class: "muted" })),
    h("div", { id: "assemble-error" }));
}

function updateAssemble() {
  const button = $("assemble");
  if (!button) return;
  const blocked = missing();
  const fetching = state.vendor.status === "fetching" && !state.own;
  button.disabled = state.busy || fetching || blocked.length > 0;
  button.closest("section")?.setAttribute("aria-busy", state.busy ? "true" : "false");
  $("assemble-status").textContent = state.busy ? "Assembling…"
    : fetching ? "Waiting for the vendor download…"
    : blocked.length ? `Add ${list(blocked)} to continue.`
    : `Signs with ${state.key ? `your key ${state.key.name}` : "a throwaway key"}.`;
}

async function assemble() {
  const generation = state.gen;
  const inputGeneration = state.inputGen;
  // Everything the outputs will claim comes from this snapshot.
  const snapshot = {
    signer: state.key?.name ?? null,
    donor: state.own ? `your file ${state.own.name}` : state.fetched ? `${state.fetched.source.name} from ${state.fetched.source.vendor}` : null,
    note: !state.own && state.fetched?.source.note ? state.fetched.source.note : null,
  };
  const donor = (state.own ?? state.fetched)?.bytes;
  const inputs = Object.fromEntries(Object.entries(state.inputs).map(([name, file]) => [name, file.bytes.slice()]));
  state.busy = true;
  state.outputs = null;
  revokeLinks();
  keepingFocus(renderBuild);
  $("assemble")?.focus();
  const stale = () => generation !== state.gen || inputGeneration !== state.inputGen;
  try {
    const result = await call("assemble", { token: state.token, donor: donor?.slice(), inputs, key: state.key?.bytes.slice() });
    if (stale()) return;
    const files = [];
    for (const [name, data] of result) {
      files.push({ name, data, sha: await sha256(data) });
      if (stale()) return;
    }
    const region = biosRegion(files);
    if (region) files.push({ ...region, sha: await sha256(region.data) });
    if (stale()) return;
    state.busy = false;
    state.outputs = { files, ifd: Boolean(region), ...snapshot };
    renderBuild();
    announce(`Assembled the ${state.build.board} image`);
    document.querySelector(".primary-output a.button")?.focus({ preventScroll: true });
    scrollTo(document.querySelector(".outputs"));
  } catch (error) {
    if (stale()) return;
    state.busy = false;
    renderBuild();
    $("assemble")?.focus({ preventScroll: true });
    const fallback = state.own && state.fetched
      ? h("button", { type: "button", class: "quiet", onclick: () => { state.own = null; filesChanged(`Using ${state.fetched.source.name}`); $("assemble")?.focus(); } },
        `Remove ${state.own.name} and use the ${state.fetched.source.vendor} download`)
      : null;
    fill($("assemble-error"), h("div", { class: "inline-error", role: "alert" }, icon("alert"),
      h("div", {}, h("p", {}, explain(error.message), " ", h("code", {}, error.message)), fallback)));
  } finally {
    if (generation === state.gen && state.busy && stale()) {
      state.busy = false;
      keepingFocus(renderBuild);
    }
  }
}

// The Intel flash descriptor layout of the build, if any: [[kind, offset, size], ...].
function ifdLayout(value) {
  if (!value || typeof value !== "object") return null;
  if (Array.isArray(value.IntelIfd)) return value.IntelIfd;
  for (const child of Object.values(value)) {
    const found = ifdLayout(child);
    if (found) return found;
  }
  return null;
}

// fstart fills only the BIOS region of an Intel flash image; the descriptor,
// GbE and ME regions are left erased. The BIOS region alone lets the user
// keep their board's own regions.
function biosRegion(files) {
  const layout = ifdLayout(state.manifest?.assembly);
  if (!layout || !layout.some(([kind]) => kind !== "Bios" && kind !== "Descriptor")) return null;
  const bios = layout.find(([kind]) => kind === "Bios");
  const image = files.find(f => f.name.endsWith(".pflash"));
  if (!bios || !image || image.data.length < bios[1] + bios[2]) return null;
  return { name: `${state.build.board}-bios-region.bin`, data: image.data.slice(bios[1], bios[1] + bios[2]) };
}

// The assembler's errors are precise but terse; say what they mean.
function explain(message) {
  if (state.build.donor_required && /vendor|PSP|EFS|donor|directory|microcode|ZIP/i.test(message)) {
    const file = state.own ? `Your file ${state.own.name}` : "The vendor BIOS";
    return `${file} doesn't match this board's firmware layout. Check that it is a BIOS image for this board.`;
  }
  if (/signing key|PKCS#8|pkcs8/i.test(message)) return "The signing key can't be used. It must be an Ed25519 PKCS#8 key, PEM or DER.";
  if (/capacity|too large|exceeds/i.test(message)) return "A file is too large for its place in the image.";
  return "Assembly failed.";
}

function roleOf(name) {
  return OUTPUTS.find(role => role.test(name)) ?? { text: "" };
}

function markSaved(link) {
  link.addEventListener("click", () => { if (state.outputs) state.outputs.saved = true; });
  return link;
}

function outputs() {
  const { files, signer, donor, note, ifd } = state.outputs;
  const describe = file => {
    if (ifd && file.name.endsWith(".pflash")) return "The whole flash layout, but only its BIOS region is filled.";
    const { text } = roleOf(file.name);
    return typeof text === "function" ? text(file.name, files) : text;
  };
  // The first file whose role is a complete image, in role order.
  const primaryRole = OUTPUTS.findIndex(role => role.primary && files.some(f => role.test(f.name)));
  const primary = primaryRole >= 0 ? files.find(f => OUTPUTS[primaryRole].test(f.name)) : null;
  const rest = files.filter(f => f !== primary)
    .sort((a, b) => OUTPUTS.indexOf(roleOf(a.name)) - OUTPUTS.indexOf(roleOf(b.name)));
  const provenance = [
    signer ? `Signed with your key ${signer}` : "Signed with a throwaway key, so the hash differs each time you assemble",
    donor ? `vendor firmware from ${donor}` : null,
  ].filter(Boolean).join("; ");
  return h("section", { class: "outputs", "aria-labelledby": "outputs-title" },
    h("h2", { id: "outputs-title" }, "Your image"),
    state.outputs.outdated ? h("p", { class: "notice warn compact" }, icon("alert"),
      h("span", {}, "Assembled before your last file change. Assemble again to include it; this image is still available below.")) : null,
    primary ? h("div", { class: "primary-output" },
      h("div", {},
        h("p", { class: "out-name" }, primary.name),
        h("p", { class: "muted" }, `${describe(primary)} ${bytes(primary.data.length)} · ${exact(primary.data.length)}.`),
        hash(primary.sha, `${primary.name} SHA-256`, true, true),
        h("p", { class: "muted" }, `${provenance}.`),
        note ? h("p", { class: "vendor-note" }, icon("alert"), h("span", {}, note)) : null,
        ifd ? h("p", { class: "vendor-note" }, icon("alert"), h("span", {},
          "Only the BIOS region of this image is filled; its flash descriptor, GbE (your MAC address) and ME regions are empty. Write just the BIOS region, for example ",
          h("code", {}, `flashrom -p internal --ifd -i bios -w ${primary.name}`),
          ", or the BIOS-region file below.")) : null),
      markSaved(saveLink(primary.name, primary.data))) : null,
    h("ul", { class: "files" }, rest.map(file => h("li", {},
      h("div", {},
        h("p", { class: "out-name" }, file.name, h("span", { class: "muted" }, ` · ${bytes(file.data.length)}`)),
        h("p", { class: "muted" }, describe(file)),
        hash(file.sha, `${file.name} SHA-256`, true, true)),
      markSaved(saveLink(file.name, file.data, "Save", true))))));
}

function binaries(build) {
  const list = (build.binaries ?? []).slice().sort();
  if (!list.length) return null;
  const items = list.map(path => {
    const button = h("button", { type: "button", class: "small", "aria-label": `Save ${path}` }, icon("download"), "Save");
    button.addEventListener("click", async () => {
      const generation = state.gen;
      button.disabled = true;
      try {
        const data = await call("binary", { token: state.token, path });
        if (generation !== state.gen) return;
        const ci = path.startsWith("/images/");
        const link = saveLink(ci ? `ci-${path.split("/").pop()}` : path, data, "Save", true);
        link.setAttribute("aria-label", `Save ${path}`);
        button.replaceWith(link);
        link.focus();
        link.click();
      } catch (error) {
        if (generation !== state.gen) return;
        button.disabled = false;
        button.textContent = "Retry";
        announce(error.message);
      }
    });
    const ci = path.startsWith("/images/");
    const size = state.manifest?.files?.[path]?.size;
    return h("li", {}, h("div", {}, h("code", {}, path, size != null ? h("span", { class: "muted" }, ` · ${bytes(size)}`) : null),
      ci ? h("p", { class: "muted" }, `Assembled by CI with a throwaway key; saved as ci-${path.split("/").pop()}.`) : null), button);
  });
  return h("details", { class: "binaries", open: Boolean(unusable(build)) },
    h("summary", {}, `Compiled binaries (${list.length})`),
    h("p", { class: "muted" }, "What CI compiled for this build: each stage as ELF and flat binary, and SMM images."),
    h("ul", { class: "files compact" }, items));
}

// --- Wiring ----------------------------------------------------------------------

$("release").addEventListener("change", event => {
  if (!event.target.value) return;
  if (!leaveUnsaved()) {
    renderReleaseOptions();
    return;
  }
  selectRelease(event.target.value);
});
$("filter").addEventListener("input", renderGallery);
$("filter").addEventListener("keydown", event => {
  if (event.key !== "Enter") return;
  const first = $("gallery").querySelector(".card");
  if (!first) return;
  // A payload named in the query ("x60 coreboot") picks that payload.
  const terms = $("filter").value.trim().toLowerCase().split(/\s+/);
  const payload = terms.find(t => PAYLOADS[t]);
  if (payload) storage("fstart.payload", payload);
  chooseBoard(first.id.replace(/^card-/, ""), payload);
});
window.addEventListener("hashchange", route);
document.addEventListener("keydown", event => {
  const target = document.activeElement;
  const typing = target?.tagName === "TEXTAREA" || target?.tagName === "SELECT" || (target?.tagName === "INPUT" && !["radio", "checkbox", "button", "file"].includes(target.type));
  if (event.key === "/" && !typing) {
    event.preventDefault();
    if (state.board) location.hash = "#/";
    $("filter").focus();
  }
});
$("open-bundle").addEventListener("click", () => $("bundle-file").click());
$("intro-open").addEventListener("click", () => $("bundle-file").click());
$("bundle-file").addEventListener("change", event => {
  const file = event.target.files[0];
  event.target.value = "";
  if (file) openLocalBundle(file);
});

// A file dropped next to a drop zone must not replace the page.
for (const type of ["dragover", "drop"]) window.addEventListener(type, event => event.preventDefault());
window.addEventListener("beforeunload", event => {
  if (state.outputs && !state.outputs.saved) event.preventDefault();
});

(async () => {
  state.config = await loadConfig();
  if (!state.config.proxy) {
    $("foot-note").textContent = "Images are assembled locally, from the bundle file you open.";
  }
  await loadReleases();
})();
