# Stage allocation

Early stages must work without a global allocator. A stage with `heap_size: None`
or zero has no `_FSTART_HEAP`, `_FSTART_HEAP_SIZE`, or `.fstart.heap` in its linker
script. The stage allocator retains a relocation to `_FSTART_HEAP` in
`.fstart.keep`, which every generated stage script keeps even with LTO and
section garbage collection. Accidentally selecting an allocator for a heapless
stage therefore fails linking, even when it is never called. Importing Rust's
`alloc` without selecting an allocator fails compilation instead.

`fstart-ffs` provides allocation-free root verification, directory views and
LZ4 decompression. `fstart-load` reads, verifies, measures, decompresses and
places bootstrap images using caller-owned or platform-reserved memory.
Compressed input scratch is part of that explicit load reservation, not a heap.
Early flows use `fstart-stage/bootstrap` (and signature verification where
required), not the mainstage `fstart-stage/ffs` services.

Heap-using stages select `fstart-stage/alloc`, the single global allocator backed
by `linked_list_allocator`. It initializes lazily over the linker-owned arena
and reclaims dropped temporary buffers. Mainstage `ffs` services imply this
feature: their authenticated directory cache, retained assets and load ledger
own memory. The complete arena remains reserved until handoff; reclaiming Rust
buffers does not return the arena to the OS or EFI memory map.

Libraries gate heap-dependent APIs/imports on their own capabilities. Intel
board ACPI is an optional capability selected only for the ramstage compilation
unit; host DSDT tests enable it through dev-dependencies. Fixed-buffer paging
remains independent of heap-backed paging. Cargo features are additive: a
workspace-wide check with unified features is not evidence that an early stage
is heapless. Validate the actual separate compilation units through `fbuild`.

The allocator uses a spin lock. Interrupt, exception and panic/logging paths
must not re-enter it while interrupted code holds the lock. On x86, caching
must be enabled before allocation (locked RMW can hang with `CR0.CD=1` on i945).
Keep permanent directory/policy allocations separate from payload churn where
possible, and do not reduce heap sizes based on reclamation alone.

Sunxi bootblocks share their small SRAM budget with code, data and stack. Do not
replace removed heap dependencies with large static or stack buffers. Keep the
size-optimized early loading path and inspect `fbuild build --size-report` when
changing it. Linker capacity checks, real release links, and the linker tests
(including a deliberately missing heap under GC) protect this boundary.
