# Firmware Updates

Design for authenticated, power-fail-safe firmware updates in fstart using
RO/RW slot selection, UEFI Capsule-on-Disk via CrabEFI, and optional TPM 2.0
anti-rollback protection.

## Goals and Constraints

- **RO/A/B and RO/A** from one codebase: RO/A/B gives automatic fallback to the
  previously confirmed slot; RO/A trades the second RW slot for flash space and
  falls back to RO recovery.
- **Pre-RAM RO -> RW handoff** on x86 servers, workstations, and laptops so
  raminit/memory init is updatable in the field. SRAM-constrained SoCs where
  pre-RAM selection is infeasible are explicitly out of scope.
- **Identical RW_A and RW_B images**: each RW slot is a self-contained FFS
  image with its `Root` at byte 0 of the slot and all offsets relative to the
  slot base, so one signed update capsule flashes into either slot.
- **UEFI Capsule-on-Disk (CoD) first, CrabEFI only** (no EDK2).
- **No SMM required initially**, with hardware SPI write-protection locking
  before any third-party EFI code executes. The flash formats, RO selector, and
  slot-state journal are designed so an optional SMM writer/variable service
  can be added later without breaking the on-flash ABI.
- **Optional TPM 2.0 rollback protection**, enforcing a monotonic security
  floor in RO without ever bricking a failed trial boot.

## Flash Layout

```text
+-------------------------------------------------------------------+
| RO (hardware write-protected)                                     |
|   Reset vector / silicon headers (FIT, initial ucode, PSP, ...)   |
|   RO bootblock + TrustBlock (.rodata.fstart_trust) + slot table   |
|   RO recovery FFS (optional minimal or full CrabEFI recovery)     |
+-------------------------------------------------------------------+
| SLOT_STATE_0 (4 KiB, erase-aligned)                               |
| SLOT_STATE_1 (4 KiB, erase-aligned)                               |
+-------------------------------------------------------------------+
| RW_A (self-contained FFS image, Root at slot offset 0)            |
+-------------------------------------------------------------------+
| RW_B (optional; identical format/size to RW_A)                    |
+-------------------------------------------------------------------+
| Persistent data (outside slots)                                   |
|   EFI variable store (CrabEFI varstore)                           |
|   Memory training cache (MRC / APOB)                              |
+-------------------------------------------------------------------+
```

- **Slot geometry is RO-owned**: slot offsets, sizes, and permitted CAR/RAM
  load windows live in RO's board layout, never in the capsule or RW image.
- **RO/A vs RO/A/B**: the RO slot table simply has 1 or 2 `(offset, size)`
  entries. Note that RO/A only avoids bricking on a power-cut mid-flash if RO
  contains a self-contained recovery path (DRAM init + CrabEFI + storage/USB
  drivers) capable of re-flashing `RW_A` without reading any bytes from `RW_A`.
- **Early SPI and cache speedups before verification (real hardware):** on
  physical x86 platforms, RO enables fast SPI reads (e.g. Intel SPI
  prefetch/caching and AMD FCH SPI100) and maps the SPI flash MMIO window with
  Write-Protected (WP) MTRRs *before* running Ed25519 / SHA-256 verification on
  `RW_A`/`RW_B`. This keeps cold-boot verification latency in the tens of
  milliseconds instead of uncached SPI speeds.
- **Silicon-fixed regions stay outside RW slots**:
  - *Intel*: reset vector, FIT pointer/table, reset-time microcode, and ME/TXE
    regions remain in RO/IFWI. RW may carry newer microcode loaded early by the
    RW pre-RAM stage.
  - *AMD*: EFS and the vendor PSP/BIOS directory structure stay outside x86 RW
    slots, with optional alignment to AMD PSP ISH A/B recovery directories
    (PSP directory entry types `0x48` / `0x4A` for L2A/L2B and BIOS directory
    `0x49`) when PSP A/B firmware updates are also enabled; x86 fstart stages
    live in `RW_A`/`RW_B` and load directly into fixed DRAM trained by PSP.

## RW FFS Image Format and Handoff

fstart already has the core primitives in `crates/core/src/ffs` and
`crates/ffs`:

- `TrustBlock` (`crates/core/src/ffs/trust.rs`) in RO replaces coreboot's GBB:
  it holds `image_family`, `minimum_security_version`, and up to 4 Ed25519
  `VerificationKey`s.
- `Root` (`crates/ffs/src/root.rs`, 512 bytes, `FSTARTBR`) replaces coreboot's
  `VBLOCK` + CBFS metadata hash: it is Ed25519-signed and pins
  `security_version`, `image_family`, `key_id`, `BootstrapDescriptor`s, and
  `DirectoryRef` (SHA-256 of the `FSMZ` directory).

Small extensions needed for RW slots:

1. **Slot-relative media view**: `crates/stage/src/root.rs` currently rejects
   `anchor.image_offset() != 0`. Wrap the selected slot in a bounded
   slot-relative `BootMedia` slice (offset 0 = start of chosen slot) and carry
   the selected slot ID, slot bounds, and authenticated `Root` SHA-256 through
   stage handoff (`install_locator` + store/handoff record).
2. **Pre-RAM RW bootstrap role**: add a `Romstage` (pre-RAM) role to
   `BootstrapRole` in `crates/ffs/src/root.rs` (currently only `Postcar` and
   `Mainstage`).
3. **Fixed execution address for identical A/B images**: an absolute-linked XIP
   pre-RAM stage cannot run from two different flash offsets. RO copies (and
   optionally decompresses) the verified RW pre-RAM stage into CAR/SRAM at a
   fixed `load_addr` (validated against RO's allowed CAR/SRAM window) and jumps
   to it. On platforms where DRAM is already trained before x86 runs (e.g. AMD
   Zen PSP), the stage loads directly to fixed DRAM.
4. **ESRT release version**: store a `u32` firmware release version (for UEFI
   ESRT/FMP) in `RootWire`'s 20 reserved bytes inside the signed area, keeping
   it distinct from `security_version: u64`.

## Slot-State Journal (`SLOT_STATE_0` / `SLOT_STATE_1`)

RO is the sole boot selector and needs durable, power-fail-safe metadata that
does not live in the EFI variable store (because a broken raminit never reaches
CrabEFI, and without SMM the varstore may be exposed differently).

### Storage Model: Append-Only 128-Byte Records, CrabEFI-Managed Rollover

Reserve **two 4 KiB erase-aligned NOR flash sectors** (`SLOT_STATE_0` and
`SLOT_STATE_1`). Each sector holds 32 fixed **128-byte records**.

- **Why 128-byte append-only records instead of 1-byte `1 -> 0` bit burns?**
  Standard SPI NOR (Winbond/Macronix/GigaDevice) allows re-programming
  individual bytes in a page, but ECC-protected NOR flashes (e.g. Infineon
  Semper with 16-byte ECC lines) forbid programming the same line twice.
  Appending a fresh 128-byte record whose body + CRC32C occupy `0x00..0x70`
  and whose commit byte (`0x5A`) lives on its own dedicated 16-byte trailing
  commit line (`0x70..0x80`) works on every NOR part and keeps every metadata
  byte under CRC32C.
- **Why RO pre-RAM stays tiny (no erase code in RO, 128-byte stack):**
  When CrabEFI writes a `Trial(N)` record during capsule install, CrabEFI
  (running in full DRAM with the full SPI driver) checks whether the active
  sector has at least `N + 2` free 128-byte records remaining. If not,
  **CrabEFI** erases the inactive sector and writes the `Trial(N)` record
  there. RO pre-RAM stream-scans one 128-byte record at a time (128 bytes of
  stack) and only ever programs the next erased slot—never erasing.

### Record Layout (128 Bytes, Little-Endian, Format Version 2)

| Offset | Size | Field | Description |
|---:|---:|---|---|
| `0x00` | 4 | `magic` | `b"FSSL"` |
| `0x04` | 1 | `version` | Format version (`2`) |
| `0x05` | 1 | `phase` | `Stable = 1`, `Installing = 2`, `Trial = 3`, `FloorPending = 4`, `Recovery = 5`, `Syncing = 6` |
| `0x06` | 1 | `active_slot` | `SlotA = 0`, `SlotB = 1`, `Recovery = 0xff` |
| `0x07` | 1 | `fallback_slot` | `SlotA = 0`, `SlotB = 1`, `Recovery = 0xff` |
| `0x08` | 1 | `tries_left` | Remaining trial attempts (e.g. starts at `3`) |
| `0x09` | 1 | `flags` | Bit 0: `confirmed`, Bit 1: `sync_done` |
| `0x0a` | 2 | `reserved` | Zero |
| `0x0c` | 4 | `fw_version` | Active/candidate ESRT `u32` release version |
| `0x10` | 8 | `seq` | Strictly monotonic sequence number |
| `0x18` | 8 | `sec_version` | Active/candidate `security_version` |
| `0x20` | 4 | `last_attempt_ver` | ESRT `LastAttemptVersion` |
| `0x24` | 4 | `last_attempt_status` | ESRT `LastAttemptStatus` (standard UEFI values) |
| `0x28` | 32 | `root_digest` | Full SHA-256 of active/candidate `Root` (binds active slot to image; fallback admission is signature + floor checked) |
| `0x48` | 36 | `reserved_body` | Zero |
| `0x6c` | 4 | `crc32c` | CRC32C over bytes `0x00..0x6c` |
| `0x70` | 15 | `commit_pad` | `0xff` (dedicated 16-byte commit line) |
| `0x7f` | 1 | `commit` | Programmed last (`0x5a`) after bytes `0x00..0x70` are written and verified |

### Read, Write, and Power-Loss Rules

- **Read (in RO):** scan all 32 slots of **both** sectors (never stop at the
  first hole). Accept records with valid `magic`, `version`, `commit == 0x5a`,
  and `crc32c`. Select the record with the highest `seq`. If neither sector has
  a valid record (blank/corrupted flash), fall back to trying `RW_A` -> `RW_B`
  -> RO recovery (or recovery directly on provisioned production boards).
- **Append:** program bytes `0x00..0x70` into the next fully erased (`0xff`)
  128-byte slot, read back, then program the dedicated commit line (`0x70..0x80`,
  with `commit = 0x5a` at `0x7f`) and verify. A power cut
  at any point leaves an invalid record that is ignored on reboot, preserving
  `seq - 1`.
- **Sector rollover (CrabEFI only):** never erase the sector containing the
  highest valid `seq`. Erase the *other* sector, verify erased, append
  `seq + 1` with `commit = 0x5a` there, and verify. Only after the new sector
  holds a valid `seq + 1` may the old sector be reclaimed on a future rollover.
- **Wear:** normal stable boots write **zero bytes**. Writes occur only on
  capsule install, trial debit, confirmation, and fallback.

## Boot Flow

### Cold Boot (RO Pre-RAM Selector)

1. Hardware RO write-protect is already in effect; RO locks any software RO
   protection registers early.
2. Read the latest valid record from `SLOT_STATE_{0,1}`.
3. If TPM rollback protection is enabled:
   - Read the platform NV rollback index.
   - If the journal is in `FloorPending`, re-authenticate the confirmed slot's
     `Root` (and in A/B, confirm both slots meet the new floor), advance the
     TPM NV index to `confirmed.sec_version`, read back, and append `Stable`.
   - Issue `TPM2_NV_WriteLock` (reset-scoped) and verify lock succeeded before
     running any RW code. If TPM is required by policy and missing/unreadable,
     boot RO recovery rather than treating the floor as 0.
4. Compute `effective_floor = max(trust.minimum_security_version, tpm_nv_floor)`.
5. Select slot:
   - **`Stable` / `FloorPending`**: verify `active_slot`. If verification fails,
     fall back to `fallback_slot`, then RO recovery.
   - **`Trial`**:
     - If `tries_left > 0`: **append `tries_left - 1` to `SLOT_STATE` before
       jumping to RW** (if the debit write fails, do not jump). Arm platform
       watchdog (sized to allow first-boot server DRAM retraining). Verify
       `active_slot`; on verification failure, fall back immediately.
     - If `tries_left == 0`: trial exhausted. Append fallback to `fallback_slot`
       with ESRT `LastAttemptStatus = UNSUCCESSFUL` (or `Recovery` in RO/A) and
       boot the fallback.
   - **`Installing` / `Recovery`**: boot RO recovery (RO/A) or `fallback_slot`
     (RO/A/B, since the previous slot is untouched).
6. Authenticate the selected slot's 512-byte `Root` against `TrustBlock` and
   `effective_floor`, stream-verify the pre-RAM `BootstrapDescriptor` into the
   fixed CAR/SRAM load address (never loading the full directory or whole slot
   into CAR), and hand off slot bounds + authenticated `Root` digest to RW.

### S3 Resume

S3 is a continuation, not a new boot:
- RO must select the **exact same slot** as the cold boot (pinned via an
  RTC-well CMOS byte + verified against the expected `Root` digest).
- Never install capsules, decrement trial counters, confirm a trial, or advance
  the TPM floor on S3 resume.
- Re-verify the RW pre-RAM stage from flash, re-apply SPI flash locks and TPM
  platform/NV locks before jumping to the OS waking vector.
- If the pinned slot identity or resume context cannot be validated, discard S3
  and perform a cold reset.

## UEFI Capsule-on-Disk in CrabEFI (No SMM Initially)

The pinned CrabEFI (`crabefi-core`) already implements CoD scanning, FMP
parsing, PKCS#7 verification, ESRT, and `platform::CapsuleBackend` under
`efi/capsule/{disk,auth,fmp,apply,trial,result}.rs`. fstart integrates with it
rather than reimplementing FMP/FAT parsing:

1. **OS staging (`fwupd` / LVFS):**
   - fstart/CrabEFI publishes one stable board/product **ESRT system-firmware
     class GUID** (not per-slot) and advertises
     `EFI_OS_INDICATIONS_FILE_CAPSULE_DELIVERY_SUPPORTED` in
     `OsIndicationsSupported`.
   - `fwupd` matches the ESRT GUID, writes the `.cap` file to
     `\EFI\UpdateCapsule\` on the ESP, sets the file-capsule bit in
     `OsIndications`, and triggers a warm reboot.
   - **Deferred variable buffer:** enable CrabEFI's warm-reset-retained
     deferred buffer in `crates/boot/src/crabefi/runtime.rs` (currently
     `DeferredBufferConfig::disabled()`) so runtime `SetVariable(OsIndications)`
     survives warm reboot without SMM.
2. **Pre-driver capsule application in CrabEFI:**
   - CrabEFI starts with **built-in drivers only** (no option ROMs, no external
     EFI drivers, no shell, no bootloader).
   - It replays the deferred variable buffer, sees `OsIndications`, and scans
     the boot ESP for `\EFI\UpdateCapsule\`.
   - **Dual verification (kept for compatibility with stock CrabEFI):**
     1. CrabEFI validates the FMP header + **PKCS#7 signature** against the
        trusted update certificate (rejecting embedded drivers).
     2. fstart's `CapsuleBackend` validates the inner RW FFS **`Root` (Ed25519)**
        against RO's `TrustBlock`, `image_family`, `effective_floor`, and slot
        size *before* erasing anything.
   - **Install target:** `CapsuleBackend` exposes one logical `"RW_SLOT"`
     target resolved from RO's handoff (never a capsule-chosen flash address)
     and returns `platform_owns_trial() == true` so CrabEFI skips its own
     EFI-variable trial state machine in favor of RO's journal.
     - *RO/A/B*: append `Installing` to `SLOT_STATE` (rolling over sector if
       fewer than `tries + 3` slots remain), erase & program the inactive slot,
       read back and verify, append `Trial(tries = 3)`, clear `OsIndications`,
       and cold-reset.
     - *RO/A*: do not erase `RW_A` while running from `RW_A`; updates apply
       only from RO recovery (`running_slot == Recovery`), which verifies full
       readback and appends `Trial` directly.
3. **Hardware SPI lockdown (normal boots and post-failure):**
   - Before CrabEFI dispatches **any** third-party EFI image, option ROM, or
     boot manager (not at `ExitBootServices`), fstart locks SPI flash in
     hardware (`PR0..PR4` + `FLOCKDN` on Intel, `RomProtect` on AMD) covering
     RO, `SLOT_STATE_{0,1}`, `RW_A`, and `RW_B`, and reads back the lock bits.
   - **No-SMM variable store trade-off:** without SMM, if the EFI variable
     store remains unlocked at runtime, the OS can write directly to it over
     SPI (including Secure Boot keys). Locking the variable store too and
     relying exclusively on the RAM deferred buffer protects Secure Boot keys
     at the cost of losing runtime EFI variable writes across a full power-off
     (warm reboot for `fwupd` still works). Boards must explicitly choose this
     policy.

### Future SMM Path

Because RO owns the slot table, `SLOT_STATE_{0,1}`, and TPM floor, and CrabEFI
talks to flash through `CapsuleBackend`, adding SMM later is an additive
change:
- An fstart SMM handler (`crates/smm`) can own SPI writes for the EFI variable
  store (solving power-off variable persistence + Secure Boot key protection
  simultaneously) and optionally mediate capsule flashing.
- Neither the RW FFS image format, `SLOT_STATE` journal, RO boot selector, nor
  TPM anti-rollback logic needs to change.

## Boot Confirmation and Anti-Rollback Policy

### Confirmation ("Mark Slot Good")

- **Default (firmware confirmation):** when a `Trial` boot reaches CrabEFI's
  lockdown point—meaning RW raminit, postcar, ramstage, CrabEFI, and boot
  target discovery all succeeded—CrabEFI marks the slot confirmed in
  `SLOT_STATE` (and disables CrabEFI's redundant EFI-variable retry counter so
  RO's journal is the sole authority).
- **Optional (OS confirmation):** for managed systems, policy can defer
  confirmation until userspace signals health (e.g. via `CrabEFIFirmwareConfirm`
  in the deferred variable buffer from a small systemd/fwupd health service),
  consumed on the next boot before trial exhaustion is checked.

### Optional TPM 2.0 Rollback Floor (Sync-Before-Bump)

- **TPM NV index:** an 8-byte platform-hierarchy NV index created with
  `TPMA_NV_WRITESTCLEAR` (requiring `NV_DefineSpace`, `NV_Read`, `NV_Write`,
  and `NV_WriteLock` in `crates/driver-tpm`).
- **Normal updates (`confirmed.sec_version == effective_floor`):**
  - Confirm `active_slot` as `Stable`. Do **not** touch the inactive slot or
    TPM NV. This preserves the previous working release in the other slot as an
    A/B fallback with zero extra flash wear.
- **Security updates (`confirmed.sec_version > effective_floor`):**
  - *RO/A/B*: once the new slot is confirmed `Good`, CrabEFI copies the
    confirmed slot into the inactive slot (a raw byte copy since A and B are
    identical), verifies the copy against the authenticated `Root`, and appends
    `FloorPending` to `SLOT_STATE`. On the next cold boot, RO verifies the
    confirmed slot and advances the TPM NV index to `confirmed.sec_version`
    before locking it. This ensures a CVE fix actually closes rollback
    immediately after confirmation without leaving the other slot unbootable.
  - *RO/A*: once `RW_A` is confirmed `Good`, append `FloorPending`; on the next
    cold boot RO advances the TPM NV index. RO recovery itself is exempt from
    the RW floor (but refuses to install capsules below the floor).

## Bundle, CLI, and WebUI Update Assembly

Update-capable boards declare an explicit `FirmwareUpdateContract` on
`BuildPlan.update` (`crates/image-build/src/build_plan.rs`), carrying the
board's ESRT `firmware_guid`, RW `slot_size`, `ab` mode, default
`lowest_supported_version`, logical `rmap_region` (typically `"RW_SLOT"`),
and `capsule_trust_provisioned` (whether the built RO image embeds at least one
capsule PKCS#7 trust certificate). `BuildPlan::order()` validates that the
contract agrees with the RW FFS step and any `X86AbFlash` step.

`Bundle::assemble()` (`crates/image-build/src/bundle.rs`) supports two modes:

- **`AssemblyMode::Full`**: assembles the complete flash image (including RO
  and any vendor donor) and, when `plan.update` is present, also emits the
  `0xff`-padded RW slot image (`{board}-rw-slot.bin`) plus an authenticated
  UEFI 2.10 FMP capsule (`{board}.cap`) whenever capsule signing credentials
  are supplied.
- **`AssemblyMode::UpdateOnly`**: executes only the RW FFS step
  (`execute_ffs_only`) to emit `{board}-rw-slot.bin` (and `{board}.cap` if a
  signing key + certificate are supplied). On boards whose RW FFS step does not
  consume donated vendor blobs (`Manifest::update_requires_donor() == false`),
  `UpdateOnly` requires **no vendor donor ROM**, making field updates and CI
  RW-slot iteration completely donor-free.

### Two Independent Trust Checks

An installed board verifies two distinct signatures before a capsule update
boots:

1. **Outer PKCS#7 capsule signature (RSA-SHA256)**: verified by CrabEFI against
   the board's embedded capsule trust certificates before touching flash.
   Unsigned `.cap` files are intentionally never emitted; without `--capsule-key`
   and `--capsule-cert`, `Bundle::assemble()` emits `{board}-rw-slot.bin` only.
   Note that default CI bundles for `qemu-q35-ab` leave `dev-keys` disabled
   (`capsule_trust_provisioned: false`) so public development keys are never
   baked into release images by accident; for local QEMU capsule testing, build
   with `--option dev-keys=true`.
2. **Inner RW `Root` signature (Ed25519)**: verified both during CrabEFI's
   capsule preflight and on every cold boot by RO against RO's `TrustBlock`.
   Because a throwaway Ed25519 key can never match an installed board's RO
   `TrustBlock`, `Bundle::assemble()` **requires** an explicit `--signing-key`
   whenever `AssemblyMode::UpdateOnly` or `capsule_signing` is used.

### CLI (`fbuild compose-bundle`)

```bash
cargo fbuild compose-bundle \
  --bundle /path/to/board-bundle.zip \
  --update-only \
  --signing-key /path/to/board-ed25519.key.pem \
  --fw-version 5 \
  --security-version 2 \
  --capsule-key /path/to/capsule.key.pem \
  --capsule-cert /path/to/capsule.crt.pem \
  --monotonic-count 1 \
  --out /path/to/out
```

`fbuild compose-bundle` records the assembly mode, ESRT GUID, `fw_version`,
`security_version`, `ed25519_public_key`, `ephemeral_signing_key`,
`capsule_trust_provisioned`, `monotonic_count`, `lowest_supported_version`,
and SHA-256 digests of emitted outputs in `assembly-report.json`. Reusing
`--out` across runs automatically removes stale firmware outputs recorded in a
previous `assembly-report.json`.

### Browser WebUI (`tools/image-web`)

The browser assembler exposes the same `Bundle::assemble()` path over WASM:

- Catalog entries advertise update capability (`update: true`, `update_ab` in
  `catalog.json`) with an `"A/B updates"` (or `"Updates"` for RO/A) badge on
  the board card.
- Update-capable bundles show a **Firmware update & UEFI capsule** panel with
  **Full ROM** vs **Update only** mode selection, plus a notice whenever the
  bundle's RO has no capsule trust certificate provisioned
  (`capsule_trust_provisioned: false`).
- When **Update only** is selected, Intel flash-region inputs, firmware
  settings, and Secure Boot key inputs are hidden; the vendor donor dropzone is
  also hidden whenever `updateRequiresDonor` is `false`.
- Optional RSA private key (PEM/DER) and matching X.509 certificate (PEM/DER)
  inputs validate the key/certificate pair inside the WASM worker and produce
  both `{board}-rw-slot.bin` and a PKCS#7-signed `{board}.cap` locally in the
  browser—private keys never leave the user's machine. Both **Update only** and
  capsule signing require supplying the board's Ed25519 signing key.

## Implementation Roadmap

1. **FFS & stage primitives (`crates/core`, `crates/ffs`, `crates/stage`):**
   - Add `BootstrapRole::Romstage` and `fw_version: u32` to `RootWire`.
   - Support slot-relative `BootMedia` slicing (`image_offset != 0`) in
     `crates/stage/src/root.rs` and carry active slot + `Root` digest in
     handoff.
2. **Slot-state journal (new `crates/slot-state` or module in `crates/ffs`):**
   - Implement the 128-byte record format, two-sector scanner, pre-RAM
     append-only writer (no erase), and CrabEFI rollover helper with unit tests
     covering torn writes, wraparound, and power-cut recovery.
3. **Host image assembly (`crates/image-build`):**
   - Support RO/A and RO/A/B layouts in the board plan: build identical RW FFS
     images linked for fixed CAR/SRAM `load_addr`, patch RO `TrustBlock` + slot
     table, and emit a signed `.cap` artifact alongside the full ROM image.
4. **RO pre-RAM selector & hardware SPI locks (`crates/platform-intel`, `crates/platform-amd`):**
   - Wire RO slot selection, CAR/SRAM bounds checks, pre-RAM try debit,
     watchdog arming, and hardware SPI range locks (`PR0..PR4`/`FLOCKDN`,
     `RomProtect`) + S3 slot pinning.
5. **CrabEFI CoD integration (`crates/boot/src/crabefi`):**
   - Enable `DeferredBufferConfig` for warm-reset variable persistence.
   - Implement `platform::CapsuleBackend` backed by RO handoff + `SLOT_STATE` +
     Ed25519 `Root` preflight verification, firmware confirmation, and
     sync-before-bump.
   - Enforce SPI lockdown before launching any third-party EFI code.
6. **Optional TPM 2.0 NV rollback (`crates/driver-tpm`):**
   - Add `NV_DefineSpace`, `NV_Read`, `NV_Write`, and `NV_WriteLock` to
     `crates/driver-tpm/src/tpm20.rs` and wire RO floor check/bump/lock.
