//! x86 coreboot payload launcher (SeaBIOS, GRUB, libpayload programs, ...).
//!
//! The payload sees what coreboot would show it: a coreboot table reachable
//! from the forwarding header at [`FORWARD_ADDR`], and a 32-bit protected
//! mode entry with the table address as argument. Files it would read from
//! CBFS arrive in RAM instead, listed in the table (see `fstart-coreboot`).
//! Everything handed over comes from verified FFS files.

use crate::{Description, Handoff, MainstagePayload};
use fstart_core::ffs::FileType;
use fstart_core::services::ffs_context;
use fstart_core::services::{FramebufferInfo, UartPort};
use fstart_coreboot::manifest::{self, Manifest};
use fstart_coreboot::{FORWARD_ADDR, Framebuffer, Serial, TableWriter};
use fstart_stage::boot::MemoryWindow;

/// Legacy BIOS shadow RAM, where SeaBIOS and other 16-bit payloads live.
const LEGACY_SHADOW: MemoryWindow = MemoryWindow {
    start: 0xc_0000,
    size: 0x4_0000,
};

/// RAM for the files handed to a coreboot payload and its table: at least
/// [`window_size`] bytes, below 4 GiB.
#[derive(Clone, Copy, Debug)]
pub struct Window {
    base: u64,
    size: usize,
}

impl Window {
    /// # Safety
    /// At handoff, `0xc0000..0x100000` decodes to writable DRAM (PAM shadow
    /// opened) that the firmware no longer uses, and `base..base + size` is
    /// RAM reserved for the launcher, excluded from the load policy and from
    /// OS-visible RAM.
    pub const unsafe fn new(base: u64, size: usize) -> Self {
        Self { base, size }
    }
}

/// RAM the platform has to set aside for the [`Window`].
#[must_use]
pub fn window_size() -> Option<usize> {
    Some(verified_manifest()?.window_size())
}

fn verified_manifest() -> Option<Manifest<'static>> {
    Manifest::decode(ffs_context::read_verified_asset(manifest::ASSET)?)
}

/// Boots the build-selected coreboot payload.
pub struct X86CorebootPayload;

impl<D> MainstagePayload<D> for X86CorebootPayload {
    fn boot(handoff: Handoff<'_>, _: D) -> ! {
        let Some(manifest) = verified_manifest() else {
            halt("no verified coreboot payload manifest");
        };
        let window = handoff
            .placement
            .coreboot_window
            .unwrap_or_else(|| halt("no table window"));
        let table = write_tables(&handoff.description, window, &manifest)
            .unwrap_or_else(|reason| halt(reason));
        let base = load_payload().unwrap_or_else(|reason| halt(reason));
        #[cfg(feature = "tcg")]
        measure(&manifest);
        fstart_log::info!(
            "coreboot payload: loaded at {:#x}, entering {:#x}",
            base,
            manifest.entry
        );
        fstart_arch::x86::boot::disable_boot_media_rom_cache_for_handoff();
        fstart_stage::timestamps::handoff();
        fstart_log::flush();
        // SAFETY: the verified image is loaded at its linked address and the
        // mainstage stack lies in the identity-mapped low 4 GiB. Nothing of
        // the firmware runs after this.
        unsafe { fstart_arch::x86::boot::protected_mode_call(manifest.entry as u32, table as u32) }
    }
}

/// Copy the files into the table window, write the table behind them and
/// point the forwarding header at it. Returns the table address.
fn write_tables(
    description: &Description<'_>,
    window: Window,
    manifest: &Manifest<'_>,
) -> Result<u64, &'static str> {
    let (window, len) = (window.base, window.size);
    if len < manifest.window_size() || window.saturating_add(len as u64) > 1 << 32 {
        return Err("table window too small or above 4 GiB");
    }
    // SAFETY: the platform reserves the window for exclusive launcher use.
    let buf = unsafe { core::slice::from_raw_parts_mut(window as *mut u8, len) };

    let mut placed = heapless::Vec::<(&str, u64, u32), { manifest::MAX_FILES }>::new();
    let mut at = 0;
    for (name, data) in manifest.files() {
        buf[at..at + data.len()].copy_from_slice(data);
        placed
            .push((name, window + at as u64, data.len() as u32))
            .map_err(|_| "too many payload files")?;
        at += data.len().next_multiple_of(manifest::FILE_ALIGN);
    }

    let table_addr = window + at as u64;
    let mut writer = TableWriter::new(&mut buf[at..]).map_err(|_| "table window full")?;
    let mut tables: heapless::Vec<(u64, u64), 8> = description
        .tables
        .iter()
        .map(|range| (range.base, range.size))
        .collect();
    // The table window, and the forwarding header that shares the first page
    // with the real-mode IVT.
    tables
        .extend_from_slice(&[(window, len as u64), (0, 0x1000)])
        .map_err(|_| "too many table ranges")?;
    let full = |_| "coreboot table does not fit";
    writer
        .memory(
            description.memory.iter().map(|entry| {
                let entry = *entry;
                (entry.addr, entry.size, entry.kind)
            }),
            &tables,
        )
        .map_err(full)?;
    let (vendor, part) = description.mainboard;
    writer.mainboard(vendor, part).map_err(full)?;
    if let Some(port) = description.uart {
        writer.serial(&serial(&port)).map_err(full)?;
    }
    if let Some(fb) = description.framebuffer {
        writer.framebuffer(&framebuffer(&fb)).map_err(full)?;
    }
    if let Some(acpi) = description.acpi {
        writer.acpi_rsdp(acpi.rsdp).map_err(full)?;
    }
    for &(name, address, size) in &placed {
        writer.file(name, address, size).map_err(full)?;
        fstart_log::info!(
            "coreboot payload: {} at {:#x} ({} bytes)",
            name,
            address,
            size
        );
    }
    let table_len = writer.finish().len();
    fstart_log::info!(
        "coreboot table: {} bytes at {:#x}",
        table_len as u32,
        table_addr
    );

    // SAFETY: page zero is reserved from OS RAM and unused by the firmware;
    // coreboot keeps its forwarding header at the same place.
    let forward = unsafe { core::slice::from_raw_parts_mut(FORWARD_ADDR as *mut u8, 0x40) };
    let mut writer = TableWriter::new(forward).map_err(full)?;
    writer.forward(table_addr).map_err(full)?;
    writer.finish();
    Ok(table_addr)
}

fn serial(port: &UartPort) -> Serial {
    Serial {
        kind: if port.io_port {
            fstart_coreboot::SERIAL_IO_MAPPED
        } else {
            fstart_coreboot::SERIAL_MEMORY_MAPPED
        }
        .into(),
        base: (port.base as u32).into(),
        baud: port.baud.into(),
        regwidth: port.reg_stride.into(),
        input_hertz: port.clock_hz.into(),
    }
}

fn framebuffer(info: &FramebufferInfo) -> Framebuffer {
    Framebuffer {
        physical_address: info.base_addr.into(),
        x_resolution: info.width.into(),
        y_resolution: info.height.into(),
        bytes_per_line: (info.stride * u32::from(info.bits_per_pixel).div_ceil(8)).into(),
        bits_per_pixel: info.bits_per_pixel,
        red_mask_pos: info.red_pos,
        red_mask_size: info.red_size,
        green_mask_pos: info.green_pos,
        green_mask_size: info.green_size,
        blue_mask_pos: info.blue_pos,
        blue_mask_size: info.blue_size,
        ..Default::default()
    }
    .with_reserved_mask()
}

/// Load the verified payload image; returns its load address.
fn load_payload() -> Result<u64, &'static str> {
    // SAFETY: the context contract makes the legacy shadow loadable RAM.
    unsafe { fstart_stage::directory::grant_load_window(LEGACY_SHADOW) }
        .map_err(|_| "cannot open the legacy BIOS shadow for loading")?;
    let context = ffs_context::memory_mapped().ok_or("no mounted boot media")?;
    let size = usize::try_from(context.image_size).map_err(|_| "boot media too large")?;
    // SAFETY: the mainstage published this mapped, verified firmware window.
    let media = unsafe {
        fstart_core::services::boot_media::MemoryMapped::from_raw_addr(context.image_base, size)
    };
    fstart_stage::load_ffs_file_entry_by_type(&media, FileType::Payload)
        .ok_or("payload load or verification failed")
}

/// Measure the payload and the files handed to it into PCR 0: all of them
/// are platform firmware from the signed image. Then hand the TPM over
/// open: like on coreboot, the payload extends the separators and locks it.
#[cfg(feature = "tcg")]
fn measure(manifest: &Manifest<'_>) {
    use fstart_stage::tcg::event::{EV_POST_CODE2, post_code::POST_CODE};
    if !fstart_stage::tcg::measure_file_by_type(
        FileType::Payload,
        0,
        EV_POST_CODE2,
        Some(POST_CODE),
    ) {
        fstart_log::error!("coreboot payload: payload not measured");
        fstart_tcg::measurement_error();
    }
    for (_, data) in manifest.files() {
        // The copies handed over live in RAM: no stable blob base.
        fstart_tcg::measure_blob(0, EV_POST_CODE2, POST_CODE, 0, data);
    }
    fstart_stage::tcg::handoff(fstart_stage::tcg::Handoff::Open);
}

fn halt(reason: &str) -> ! {
    fstart_log::error!("coreboot payload: {}", reason);
    fstart_arch::x86::boot::halt()
}
