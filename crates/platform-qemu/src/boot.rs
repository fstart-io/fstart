//! QEMU software-integrity boot setup. The emulator supplies the initial code
//! and hardware description; neither a hardware root nor rollback is claimed.

use fstart_core::services::ServiceError;
use fstart_stage::boot::{MemoryPolicy, MemoryWindow};
use heapless::Vec;

type Windows = Vec<MemoryWindow, 32>;

pub(crate) fn install(
    writable: &[MemoryWindow],
    extra_reserved: &[MemoryWindow],
    firmware_base: u64,
    firmware_size: u64,
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
    let firmware = fstart_stage::fixed_helpers::MemoryMappedFfs::new(
        firmware_base,
        usize::try_from(firmware_size).map_err(|_| ServiceError::InvalidParam)?,
    );
    firmware.mount()?;
    // Authenticate ROOT512 and retain its exact directory bytes in RAM.
    firmware.verify()
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
    for node in fdt
        .root()
        .children()
        .filter(|node| node.name().starts_with("memory@") || node.name() == "memory")
    {
        for reg in node
            .reg()
            .map_err(|_| ServiceError::InvalidParam)?
            .ok_or(ServiceError::InvalidParam)?
        {
            let start = reg
                .address::<u64>()
                .map_err(|_| ServiceError::InvalidParam)?;
            let size = reg.size::<u64>().map_err(|_| ServiceError::InvalidParam)?;
            if start < ram_base || size == 0 || start.checked_add(size).is_none() {
                return Err(ServiceError::InvalidParam);
            }
            writable
                .push(MemoryWindow { start, size })
                .map_err(|_| ServiceError::InvalidParam)?;
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
        for region in layout
            .regions()
            .filter(|r| matches!(r.kind, RegionKind::Writable | RegionKind::Reserved))
        {
            reserved
                .push(MemoryWindow {
                    start: region.base,
                    size: region.size,
                })
                .map_err(|_| ServiceError::InvalidParam)?;
        }
    }
    install(&writable, &reserved, firmware_base, firmware_size)?;
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
