# AMD RAS hardware tests

`amd-ras.c` checks fstart's RAS on an AMD board: the SMM machine-check
handler (`fstart_platform_amd::smm::ras`), the HEST and BERT
(`fstart_platform_amd::acpi::apei`), the PSP's EINJ on Genoa and Siena and
Rome's own EINJ, and the BMC transport. One program for SP3 (Rome, Milan),
SP4r2 (Zeppelin) and the RS SoCs (Genoa, Siena). It runs only in a
disposable RAM-only Linux on a test bench: it mounts and writes no disk.

`linux-ras/` builds that Linux: `build.sh LINUX_SOURCE OUT_DIR` makes a
tinyconfig kernel with the options in `ras.config` (MCE, EDAC, APEI with
HEST, BERT, GHES and EINJ, PCIe AER, `/dev/mem`, `/dev/cpu/*/msr`,
debugfs) and no block, network or filesystem drivers, and an initramfs of
busybox, `amd-ras` and `init`. The script refuses a kernel that could reach
a disk. Boot it as the image's payload (`cargo fbuild assemble --payload
linux --kernel OUT_DIR/bzImage --linux-bootargs '...'`) with
`msr.allow_writes=on panic=10` and the steps in `fstart.ras=`:

| Step | What it does | Pass |
|---|---|---|
| `info` | the RAS tables and HEST sources, BERT's records, the eSPI decodes and KCS ports | — |
| `smm` | every CPU: the SMI trigger I/O cycle (B2h/80h), SMM's locked thresholding counters or PFEH, cloak and deferred masks clear; the journal | `FSTART_RAS_SMM_OK` |
| `probe` | one benign event through SMM to the BMC (diagnostic SMI D1h) | `FSTART_RAS_PROBE_OK` |
| `ce:ADDR:N[:BANK]` | N correctable memory injections at ADDR through EINJ, each consumed; EDAC must count each; then BANK's counters | `FSTART_RAS_CE_OK` |
| `sp3-einj:N[:BANK]` | N of Rome's correctable injections on each socket's firmware-owned target (`amd-ras sp3-einj`), EDAC counting each; then BANK's counters | `FSTART_RAS_CE_OK` |
| `pcie:TYPE:SEG:BUS:DEV.FN` | one PCIe injection through EINJ (TYPE 40h, 80h or 100h), the function's AER status before and after; the injection must succeed and AER must report an error of that severity from that function afterwards | `FSTART_RAS_PCIE_OK` |
| `aer:SEG:BUS:DEV.FN` | a function's AER status registers | — |
| `ue:ADDR[:UMC_ADDR]` | one uncorrectable memory injection at physical ADDR, consumed by the kernel: the machine goes down (`panic=10` resets it) | next boot: `FSTART_RAS_BERT_OK` (an uncorrected or deferred BERT record with a valid address, decoded as Linux does (MCA_ADDR[55:LSB], or [56:LSB] with the LSB in MCA_STATUS where MCA_CONFIG says so) and at least line-precise: a load-store bank's physical address in ADDR's cache line, or a memory controller's at UMC_ADDR, the normalized address, if given), `FSTART_RAS_BERT_UNVERIFIED` (only records whose address cannot be compared: a memory controller's with no UMC_ADDR, or one coarser than a line) or `FSTART_RAS_BERT_MISSING` |
| `plant:CPU:BANK:STATUS[:ADDR]` | a synthetic bank error (HWCR.McStatusWrEn), then at once a warm reset | next boot: `FSTART_RAS_BERT_OK` (that bank with that status) or `FSTART_RAS_BERT_MISSING` |
| `kcs:PORT:SPACING`, `event:...`, `sel:...`, `sel-info:...` | Get Device ID, a Platform Event, an Add SEL Entry, Get SEL Info from the OS on a KCS channel the OS's driver does not use | — |
| `storm` | SMM's storm state: interrupts or polling, the periodic SMI, the reports a busy channel deferred | — |
| `burst:ADDR:N` | N correctable injections at ADDR back to back: SMM leaves interrupts for polling (its leaky bucket), polls, and interrupts again after a quiet bucket | `FSTART_RAS_STORM_OK` |
| `flood:ADDR:N` | one correctable injection at ADDR, then N uncached reads of it, each a corrected error while the error stays in DRAM: a storm at memory speed; then as `burst` | `FSTART_RAS_STORM_OK` |
| `ipmi-load:S[:MS]`, `ipmi-wait` | the OS's IPMI driver busy for S seconds in the background (Get Device ID through `/dev/ipmi0` back to back, or MS apart); `ipmi-wait` collects it: every request must complete | `FSTART_RAS_IPMI_OK` |
| `ipmi-sel` | Get SEL Info through the OS's driver | — |
| `smn:ADDR` | one SMN register, read through 00:00.0 60h/64h | — |
| `sleep:S` | wait | — |
| `off`, `hold` | power off at the end (the default), or stay up | — |

`ue` and `plant` leave a mark in the RTC's upper CMOS bank (byte E0h on,
which nothing of fstart's uses) before the machine goes down; the boot
after them finds it, checks the BERT instead of injecting or planting
again, and clears it: one injection per mark, whether the BERT has the
error or not. A run that never came back leaves the mark, and the next
run reports `FSTART_RAS_BERT_MISSING` once. The BMC side is read on the BMC: its SEL
(`ipmitool sel list`, OpenBMC's `/var/log/ipmi_sel`).

Examples, as run on the S8040 and the Onyx:

```text
fstart.ras=info,smm,probe,ce:0x140000000:12:21,smm,off
fstart.ras=info,ue:0x140000000,off
fstart.ras=info,plant:1:22:0xbc00080001010135:0x140000000,off
# The OS's IPMI driver busy on the channel SMM reports on (the Onyx), a
# report and a storm meanwhile:
fstart.ras=info,smm,ipmi-sel,ipmi-load:90,ce:0x140000000:12:22,probe,burst:0x140000000:400,storm,ipmi-wait,ipmi-sel,off
```

The kernel has the OS's IPMI driver (KCS from SPMI or its default port
`0xCA2`, `/dev/ipmi0`): the raw `kcs`, `event`, `sel` and `sel-info`
steps must not use the channel it drives.

On SP3, `amd-ras sp3-einj` injects one correctable error on every
socket's firmware-owned target (Rome's restricted EINJ); the PSP's EINJ of
the RS SoCs takes any address.

Build `amd-ras` alone statically for x86-64 Linux, e.g. with Nix's
`pkgsStatic.stdenv.cc`.

## What the checks do not prove

`smm` checks every online CPU (`/sys/devices/system/cpu/online`): one it
cannot read or run on is a failure, not a CPU left out. `ce` needs EDAC's
CE counters read before and after each injection. The diagnostic SMI
answers only the CPU that raised it; it arrives a few
instructions after the OUT, and `amd-ras` keeps its registers live and
retries. `plant` writes a status the hardware did not log: it tests the
BERT path, not the hardware's error detection. A PCIe injection that sets
an endpoint's AER status reaches the OS only through a port with AER.

Never use arbitrary addresses, CPU-cache corruption or fatal injection on
a host with valuable storage or workloads.
