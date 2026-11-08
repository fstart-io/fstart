# Firmware USB host

Reusable `no_std` **CrabUSB xHCI** integration and a read-only USB mass-storage
snapshot. It does not implement another host controller or SCSI command set:
`cotton-scsi` supplies the SCSI codecs; `bot.rs` bridges CrabUSB bulk endpoints.
AMD silicon/SMU setup and board PHY/wiring policy live outside this crate.

## Runtime and ownership contract

- The caller exclusively owns the enabled, uncached, identity-mapped controller
  BAR and a direct-mapped DMA runtime. Register envelopes are checked with the
  existing `xhci` register definitions before initialization/DMA.
- `KernelOp`/`DmaOp` and the framework `Timer` supply allocation, coherency,
  mapping, delays and microsecond deadlines. Coherency is a platform fact,
  **not** something obtained from volatile accesses or compiler fences.
- Controller actors, all DMA backing and timed-out future frames remain
  reserved **until reset**. A timeout is not cancellation or DMA release.
  Streaming transfers must always use retained bounce buffers, never let
  hardware retain a caller's stack/Vec pointer after a failed request.
- Each controller permits one cold snapshot. Failure or success consumes it;
  there is no implicit reset/reinitialization retry or buffer recycling.
- `single-executor` is an opt-in ax-sync runtime composition: one BSP,
  nonpreemptible, no USB/DMA users in IRQ/NMI/AP contexts. It implements actual
  atomic spin/RW exclusion and fails closed on unexpected contention, rather
  than hanging indefinitely or using no-op locks. Other compositions provide
  their own compatible runtime; this feature must not coexist with another
  global provider. No sleepable mutex/scheduler is provided.
- PCI interrupt delivery must remain masked. The host disables its device IRQs
  after initialization and processes completion events by polling. Do not drain
  event-register state while controller reset/CNR is active.

CrabUSB **0.11.0** and dma-api **0.10.0** share ax-sync **0.5.32**. CrabUSB 0.12's
ax-sync 0.6 dependency has incompatible signatures for identically named global
runtime hooks. Linking both ABIs to one provider is unsafe; version changes
require reviewing the **entire** dependency/runtime composition, not just a
successful `cargo check`.

## Snapshot boundary

Root-hub initialization launches asynchronous port resets. An immediate empty
change set is not proof of an empty connector. The snapshot reconciles 20 times
at 100ms intervals, with a 20s deadline for each asynchronous operation, at most
32 reported devices, and no resubmission after a transport/phase failure.
Hubs and other interfaces are identified; this is not a HID/network driver.

Only SCSI-transparent **bulk-only transport** interfaces are claimed. LUN0 is
checked for readiness, capacity is queried and **one logical block at LBA0** is
read and SHA-256 hashed. Block sizes 512..4096 are accepted. A positive
opcode/direction/CDB-length allowlist rejects OUT data and all writing/vendor
commands before even sending a CBW. CSW signature, tag, residue and status are
checked. Only readiness/Unit Attention handling is retried; transport/phase
errors poison the session. No filesystem, partition writes, format, UAS,
USB OS boot or general writable block-device API is provided.

This is a retained native bring-up lifetime, **not** a shutdown/payload handoff
contract. Keep PCI routing/BARs and DMA reservations stable; do not run a
conflicting temporary PCI bus allocator after activating the controller.
Final firmware memory maps, resource handoff and controller shutdown remain the
platform's responsibility.

```sh
cargo test -p fstart-driver-usb --features single-executor --lib
cargo clippy -p fstart-driver-usb --features single-executor --lib --tests \
  --no-deps -- -D warnings
```
