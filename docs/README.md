# fstart documentation index

Start here. `architecture.md` is the plan of record; everything else is either a
design reference for a subsystem or the evidence trail behind a decision.

## Plan of record

| Document | What it settles |
| --- | --- |
| [architecture.md](architecture.md) | Config-as-data layering, crate ownership, per-fact authority, stage structure and the acceptance gates. |

## Design reference

Current contracts for subsystems that have their own design decisions.

| Document | What it settles |
| --- | --- |
| [smm.md](smm.md) | The native SMM image format, installer, runtime block and chipset window locking. |
| [authenticated-boot.md](authenticated-boot.md) | Signature checking and static ACPI boundaries; what is and is not claimed about trust. |
| [unified-region-model.md](unified-region-model.md) | The FFS region and named-byte-range model. |
| [ide.md](ide.md) | The opt-in `fbuild ide` rust-analyzer view. |

## Evidence and measurements

Recorded results for decisions already made. Numbers here are why a decision
looks the way it does, not a build input.

| Document | What it records |
| --- | --- |
| [authenticated-boot-validation.md](authenticated-boot-validation.md) | Size baselines and boot results for the authenticated-boot work. |
| [architecture-common-plan.md](architecture-common-plan.md) | The fbuild/platform boundary, its phase2a locator-trust contracts, and the migration scope per board. Sections are marked historical inline. |

## Historical

These describe approaches that were tried and then replaced. They are kept
because they record why the current design looks the way it does, and because
hardware bring-up evidence is expensive to reproduce. **Do not implement against
them**; several contradict the plan of record.

| Document | Superseded by |
| --- | --- |
| [architecture-intel-typed-facts.md](architecture-intel-typed-facts.md) | Family-specific fbuild orchestration was replaced by the platform-plan executor in [architecture.md](architecture.md). |
| [architecture-baseline.md](architecture-baseline.md) | The metadata-profile authoring API it tracks was removed; see the migration baseline note in [architecture.md](architecture.md#implementation-and-acceptance). |
| [typed-pci-config-plan.md](typed-pci-config-plan.md) | The address/header/capability vocabulary now comes from the upstream `pci_types` crate. |

## Supersession order

Roughly newest first, so a reader arriving at a stale document can walk
forward:

```text
architecture-intel-typed-facts.md
  -> architecture-common-plan.md
    -> architecture.md
```

`architecture-baseline.md` and `typed-pci-config-plan.md` are side notes on that
chain rather than links in it.

## Adding a document

New design documents go in this directory and get a row above. Documents that
are replaced get a **Historical** row naming their successor instead of being
deleted, so existing cross-references keep resolving.
