//! QEMU software-integrity boot setup. The emulator supplies the initial code
//! and hardware description; neither a hardware root nor rollback is claimed.

use fstart_core::services::ServiceError;
use fstart_stage::boot::{MemoryPolicy, MemoryWindow};
use heapless::Vec;

type Windows = Vec<MemoryWindow, 32>;

use crate::dtb_memory::memory_visibility;

/// Fixed-layout install: mount only the image the locator describes instead
/// of the whole firmware window, so trailing erased flash never enters the
/// authenticated view.
pub(crate) fn install_packed(
    writable: &[MemoryWindow],
    extra_reserved: &[MemoryWindow],
    firmware_base: u64,
    firmware_size: u64,
) -> Result<(), ServiceError> {
    install_image(writable, extra_reserved, firmware_base, firmware_size, true)
}

fn install_image(
    writable: &[MemoryWindow],
    extra_reserved: &[MemoryWindow],
    firmware_base: u64,
    firmware_size: u64,
    packed: bool,
) -> Result<(), ServiceError> {
    let mut reserved = Windows::new();
    for window in fstart_stage::boot::running_stage_windows()
        .map_err(|_| ServiceError::InvalidParam)?
        .iter()
        .chain(extra_reserved)
    {
        reserved
            .push(*window)
            .map_err(|_| ServiceError::InvalidParam)?;
    }
    reserved
        .push(MemoryWindow {
            start: firmware_base,
            size: firmware_size,
        })
        .map_err(|_| ServiceError::InvalidParam)?;
    let policy = MemoryPolicy {
        writable,
        reserved: &reserved,
        entry_alignment: if cfg!(target_arch = "x86_64") { 1 } else { 4 },
    };
    // SAFETY: platform-discovered RAM only, with the complete live stage and
    // allocator arena, source image and hardware reservations excluded. Devices
    // have not been given DMA buffers at this point in the fixed flow.
    unsafe { fstart_stage::directory::set_load_policy(&policy) }
        .map_err(|_| ServiceError::InvalidParam)?;
    fstart_log::info!(
        "qemu: development integrity; no hardware secure boot or rollback enforcement"
    );
    // Physical bank capacity comes from the platform, never from the locator.
    // Within it, mount only the actually packed image rather than erased slack.
    let locator = fstart_stage::anchor::media_locator().ok_or(ServiceError::InvalidParam)?;
    if locator.image_offset != 0 || !locator.validate(firmware_size) {
        return Err(ServiceError::InvalidParam);
    }
    let firmware = fstart_stage::fixed_helpers::MemoryMappedFfs::new(
        firmware_base,
        // Legacy QEMU consumers retain their existing full-window contract;
        // only the fixed-layout virt flows and their consumers switch together.
        usize::try_from(if packed {
            locator.image_size
        } else {
            firmware_size
        })
        .map_err(|_| ServiceError::InvalidParam)?,
    );
    firmware.mount()?;
    // Authenticate ROOT512 and retain its exact directory bytes in RAM.
    firmware.verify()
}

/// Payload consumers reuse the exact bounded image mounted and authenticated
/// during boot setup, rather than reconstructing a bank-capacity-sized view.
#[cfg(any(feature = "linux", feature = "crabefi"))]
pub(crate) fn mounted_image() -> fstart_core::layout::Region {
    let mounted =
        fstart_core::services::ffs_context::memory_mapped().expect("QEMU boot image not mounted");
    fstart_core::layout::Region {
        kind: fstart_core::layout::RegionKind::Firmware,
        base: mounted.image_base,
        size: mounted.image_size,
    }
}

/// Static-RAM boot setup for machines whose boot ABI supplies no DTB.
///
/// QEMU sbsa-ref entered from TF-A BL31 receives x0 == 0: the secure world
/// keeps its HW_CONFIG DTB and the non-secure ABI carries no DTB pointer.
/// RAM geometry there is architectural (already in the board's closed
/// config), so the whole DTB discovery phase is skipped and the fixed RAM
/// window feeds the authenticated-load policy directly.
#[cfg(any(target_arch = "arm", target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn from_static_ram(
    ram_base: u64,
    ram_size: u64,
    firmware_base: u64,
    firmware_size: u64,
) -> Result<(), ServiceError> {
    if ram_base == 0 || ram_size == 0 || ram_base.checked_add(ram_size).is_none() {
        return Err(ServiceError::InvalidParam);
    }
    install_image(
        &[MemoryWindow {
            start: ram_base,
            size: ram_size,
        }],
        &[],
        firmware_base,
        firmware_size,
        false,
    )
}

/// QEMU's supplied DTB is machine configuration, not updatable FFS content.
/// Reject unsupported dynamic reserved-memory allocations rather than treating
/// them as free RAM. Bound the initial pointer read by the platform boot ABI.
#[cfg(any(target_arch = "arm", target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn from_dtb(
    dtb_addr: u64,
    fdt_destination: Option<u64>,
    ram_base: u64,
    firmware_base: u64,
    firmware_size: u64,
) -> Result<(), ServiceError> {
    from_dtb_with_layout(
        dtb_addr,
        fdt_destination,
        ram_base,
        firmware_base,
        firmware_size,
        None,
    )
}

/// Validate fixed build reservations against actual DTB RAM before installing
/// the existing authenticated-load policy. Other boards retain the legacy wrapper.
#[cfg(any(target_arch = "arm", target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn from_dtb_with_layout(
    dtb_addr: u64,
    fdt_destination: Option<u64>,
    ram_base: u64,
    firmware_base: u64,
    firmware_size: u64,
    layout: Option<fstart_core::layout::Layout<'_>>,
) -> Result<(), ServiceError> {
    use dtoolkit::{Node, Property, fdt::Fdt};
    if dtb_addr < ram_base || dtb_addr & 3 != 0 || usize::try_from(dtb_addr).is_err() {
        return Err(ServiceError::InvalidParam);
    }
    // SAFETY: QEMU's boot ABI supplies a readable FDT header in machine RAM.
    let size_addr = dtb_addr.checked_add(4).ok_or(ServiceError::InvalidParam)?;
    let size = u32::from_be(unsafe { core::ptr::read_unaligned(size_addr as *const u32) }) as usize;
    if !(40..=2 * 1024 * 1024).contains(&size)
        || dtb_addr
            .checked_add(size as u64)
            .and_then(|end| usize::try_from(end).ok())
            .is_none()
    {
        return Err(ServiceError::InvalidParam);
    }
    // SAFETY: bounded QEMU-provided boot blob; immutable throughout setup.
    let bytes = unsafe { core::slice::from_raw_parts(dtb_addr as *const u8, size) };
    let fdt = Fdt::new(bytes).map_err(|_| ServiceError::InvalidParam)?;
    let mut writable = Windows::new();
    // QEMU virt's BL31 destination is secure RAM (secram@e000000), not
    // normal DRAM. Discover it from the machine DTB, but only admit it while
    // the entry established Secure EL1 for this initial setup. Never infer
    // secure access merely from the target architecture or the DT property.
    #[cfg(target_arch = "aarch64")]
    let secure = fstart_arch::aarch64::booted_secure_el1();
    #[cfg(not(target_arch = "aarch64"))]
    let secure = false;
    for node in fdt.root().children().filter(|node| {
        node.property("device_type")
            .is_some_and(|p| p.value() == b"memory\0")
    }) {
        let status = node.property("status");
        let secure_status = node.property("secure-status");
        let Some(secure_only) = memory_visibility(
            status.as_ref().map(|p| p.value()),
            secure_status.as_ref().map(|p| p.value()),
            secure,
        ) else {
            continue;
        };
        for reg in node
            .reg()
            .map_err(|_| ServiceError::InvalidParam)?
            .ok_or(ServiceError::InvalidParam)?
        {
            let start = reg
                .address::<u64>()
                .map_err(|_| ServiceError::InvalidParam)?;
            let size = reg.size::<u64>().map_err(|_| ServiceError::InvalidParam)?;
            if (start < ram_base && !secure_only) || size == 0 || start.checked_add(size).is_none()
            {
                return Err(ServiceError::InvalidParam);
            }
            writable
                .push(MemoryWindow { start, size })
                .map_err(|_| ServiceError::InvalidParam)?;
            if secure_only {
                fstart_log::info!("qemu: secure boot RAM {:#x}+{:#x}", start, size);
            }
        }
    }
    let detected = MemoryPolicy {
        writable: &writable,
        reserved: &[],
        entry_alignment: 4,
    };
    if !detected.permits(dtb_addr, size as u64) {
        return Err(ServiceError::InvalidParam);
    }
    let mut reserved = Windows::new();
    reserved
        .push(MemoryWindow {
            start: dtb_addr,
            size: size as u64,
        })
        .map_err(|_| ServiceError::InvalidParam)?;
    for region in fdt.memory_reservations() {
        reserved
            .push(MemoryWindow {
                start: region.address(),
                size: region.size(),
            })
            .map_err(|_| ServiceError::InvalidParam)?;
    }
    if let Some(parent) = fdt.find_node("/reserved-memory") {
        if parent
            .property("ranges")
            .is_some_and(|property| !property.value().is_empty())
        {
            return Err(ServiceError::NotSupported);
        }
        for node in parent.children() {
            for reg in node
                .reg()
                .map_err(|_| ServiceError::InvalidParam)?
                .ok_or(ServiceError::NotSupported)?
            {
                reserved
                    .push(MemoryWindow {
                        start: reg
                            .address::<u64>()
                            .map_err(|_| ServiceError::InvalidParam)?,
                        size: reg.size::<u64>().map_err(|_| ServiceError::InvalidParam)?,
                    })
                    .map_err(|_| ServiceError::InvalidParam)?;
            }
        }
    }
    let source = MemoryWindow {
        start: dtb_addr,
        size: size as u64,
    };
    let running =
        fstart_stage::boot::running_stage_windows().map_err(|_| ServiceError::InvalidParam)?;
    let source_reserved = [
        running[0],
        running[1],
        MemoryWindow {
            start: firmware_base,
            size: firmware_size,
        },
    ];
    let source_policy = MemoryPolicy {
        writable: &writable,
        reserved: &source_reserved,
        entry_alignment: 4,
    };
    if !source_policy.permits(source.start, source.size) {
        return Err(ServiceError::InvalidParam);
    }
    if let Some(layout) = layout {
        use fstart_core::layout::RegionKind;
        let available = MemoryPolicy {
            writable: &writable,
            reserved: &reserved,
            entry_alignment: 4,
        };
        for region in layout.regions().filter(|r| {
            matches!(
                r.kind,
                RegionKind::Writable
                    | RegionKind::Execution
                    | RegionKind::Payload
                    | RegionKind::PayloadFirmware
                    | RegionKind::DeviceTree
                    | RegionKind::Reserved
            )
        }) {
            if !available.permits(region.base, region.size) {
                fstart_log::error!("resolved reservation is outside available RAM");
                return Err(ServiceError::InvalidParam);
            }
        }
        for region in layout.regions().filter(|r| {
            matches!(
                r.kind,
                RegionKind::Writable | RegionKind::Execution | RegionKind::Reserved
            )
        }) {
            reserved
                .push(MemoryWindow {
                    start: region.base,
                    size: region.size,
                })
                .map_err(|_| ServiceError::InvalidParam)?;
        }
    }
    install_image(
        &writable,
        &reserved,
        firmware_base,
        firmware_size,
        layout.is_some(),
    )?;
    if let Some(destination) = fdt_destination {
        // SAFETY: source is the exact validated, reserved QEMU DTB above.
        // Registration validates the dedicated 64 KiB destination against the
        // detected RAM policy, excluding stage/heap/media and all reservations.
        unsafe {
            fstart_stage::configure_fdt_workspace(
                source,
                MemoryWindow {
                    start: destination,
                    size: 64 * 1024,
                },
            )
        }?;
    }
    Ok(())
}
