// SPDX-License-Identifier: MIT OR Apache-2.0
//! ROM-resident Linux payload of an Rs reset image, through the common x86
//! Linux loader. The bzImage is the `rs_host::LINUX` compiler input, bound by
//! content into the reset image: no runtime disk/FFS read, no implicit file
//! and no secure-boot claim.
#[cfg(not(fstart_amd_has_rs_linux))]
compile_error!("the Linux payload requires the explicit FSTART_RS_LINUX compiler input");

/// Hand off to Linux. `image` is the immutable reset image the kernel must
/// lie in; `map` the checked final OS map (every live firmware, AP and DMA
/// allocation withheld), `zero_page` the reserved boot_params page pair.
/// `mapped` is the extent of the reset entry's identity map (1-GiB pages,
/// `PDPT_COUNT` x 512 GiB): the loader writes the zero page and copies the
/// kernel through it, so every map entry must lie below it.
/// Call on the BSP after parking the APs and quiescing firmware DMA.
#[cfg(fstart_amd_has_rs_linux)]
pub fn launch(
    map: &fstart_core::services::memory_detect::E820State,
    rsdp: u64,
    image: core::ops::Range<u64>,
    zero_page: u64,
    mapped: u64,
) -> ! {
    static KERNEL: &[u8] = include_bytes!(env!("FSTART_RS_LINUX"));
    let source = KERNEL.as_ptr() as u64;
    if source < image.start
        || source
            .checked_add(KERNEL.len() as u64)
            .is_none_or(|end| end > image.end)
    {
        fstart_log::error!("rs: Linux source escaped the immutable reserved reset image");
        fstart_arch::halt();
    }
    if map
        .entries()
        .iter()
        .any(|e| e.addr.checked_add(e.size).is_none_or(|end| end > mapped))
    {
        fstart_log::error!(
            "rs: OS memory map exceeds the identity-mapped {:#x}",
            mapped
        );
        fstart_arch::halt();
    }
    fstart_log::info!(
        "rs: Linux RAM handoff source={:#x} size={} RSDP={:#x} RAM={:#x}",
        source,
        KERNEL.len(),
        rsdp,
        map.total_ram()
    );
    for entry in map.entries() {
        let (base, size, kind) = (entry.addr, entry.size, entry.kind);
        fstart_log::info!(
            "rs: Linux e820 base={:#x} size={:#x} kind={}",
            base,
            size,
            kind
        );
    }
    fstart_log::flush();
    fstart_arch::x86::boot::boot_linux_bzimage(
        KERNEL,
        rsdp,
        map.entries(),
        env!("FSTART_RS_LINUX_ARGS"),
        zero_page,
        env!("FSTART_RS_LINUX_MTRRS") == "1",
    )
}
