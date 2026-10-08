//! x86 coreboot payload launcher (SeaBIOS, GRUB, libpayload programs, ...).
//!
//! The payload sees what coreboot would show it: a coreboot table reachable
//! from the forwarding header at [`FORWARD_ADDR`], and a 32-bit protected
//! mode entry with the table address as argument. Files it would read from
//! CBFS arrive in RAM instead, listed in the table (see `fstart-coreboot`).
//! Everything handed over comes from verified FFS files.

use crate::boot::MemoryWindow;
use crate::payload::MainstagePayload;
use fstart_core::ffs::FileType;
use fstart_core::services::FramebufferInfo;
use fstart_core::services::ffs_context;
use fstart_core::services::memory_detect::E820Entry;
use fstart_coreboot::manifest::{self, Manifest};
use fstart_coreboot::{FORWARD_ADDR, Framebuffer, Serial, TableWriter};

/// Legacy BIOS shadow RAM, where SeaBIOS and other 16-bit payloads live.
const LEGACY_SHADOW: MemoryWindow = MemoryWindow {
    start: 0xc_0000,
    size: 0x4_0000,
};

/// Platform context of the coreboot payload launcher.
///
/// # Safety
/// Implementors guarantee that, at handoff, `0xc0000..0x100000` decodes to
/// writable DRAM (PAM shadow opened) that the firmware no longer uses, and
/// that [`table_window`](Self::table_window) is RAM reserved for the
/// launcher, excluded from the load policy and from OS-visible RAM.
pub unsafe trait X86CorebootPayloadContext {
    /// Final memory map, after every firmware reservation.
    fn e820(&self) -> &[E820Entry];
    fn acpi_rsdp(&self) -> Option<u64>;
    fn framebuffer(&self) -> Option<FramebufferInfo>;
    fn serial(&self) -> Option<Serial>;
    /// Mainboard vendor and part name.
    fn mainboard(&self) -> (&str, &str);
    /// RAM for the handed-over files and the table: at least
    /// [`window_size`] bytes, below 4 GiB.
    fn table_window(&self) -> Option<(u64, usize)>;
    /// Firmware tables (ACPI, SMBIOS, the table window) payloads scan; they
    /// are reported as coreboot table memory.
    fn table_ranges(&self) -> heapless::Vec<(u64, u64), 4>;
}

/// RAM the platform has to set aside for [`X86CorebootPayloadContext::table_window`].
#[must_use]
pub fn window_size() -> Option<usize> {
    Some(verified_manifest()?.window_size())
}

fn verified_manifest() -> Option<Manifest<'static>> {
    Manifest::decode(ffs_context::read_verified_asset(manifest::ASSET)?)
}

/// Boots the build-selected coreboot payload.
pub struct X86CorebootPayload;

impl<D: X86CorebootPayloadContext> MainstagePayload<D> for X86CorebootPayload {
    fn boot(devices: D) -> ! {
        let Some(manifest) = verified_manifest() else {
            halt("no verified coreboot payload manifest");
        };
        let table = write_tables(&devices, &manifest).unwrap_or_else(|reason| halt(reason));
        let base = load_payload().unwrap_or_else(|reason| halt(reason));
        fstart_log::info!(
            "coreboot payload: loaded at {:#x}, entering {:#x}",
            base,
            manifest.entry
        );
        crate::timestamps::handoff();
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
    devices: &impl X86CorebootPayloadContext,
    manifest: &Manifest<'_>,
) -> Result<u64, &'static str> {
    let (window, len) = devices.table_window().ok_or("no table window")?;
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
    let mut tables = devices.table_ranges();
    // The forwarding header shares the first page with the real-mode IVT.
    tables
        .push((0, 0x1000))
        .map_err(|_| "too many table ranges")?;
    let full = |_| "coreboot table does not fit";
    writer
        .memory(
            devices.e820().iter().map(|entry| {
                let entry = *entry;
                (entry.addr, entry.size, entry.kind)
            }),
            &tables,
        )
        .map_err(full)?;
    let (vendor, part) = devices.mainboard();
    writer.mainboard(vendor, part).map_err(full)?;
    if let Some(serial) = devices.serial() {
        writer.serial(&serial).map_err(full)?;
    }
    if let Some(fb) = devices.framebuffer() {
        writer.framebuffer(&framebuffer(&fb)).map_err(full)?;
    }
    if let Some(rsdp) = devices.acpi_rsdp() {
        writer.acpi_rsdp(rsdp).map_err(full)?;
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
}

/// Load the verified payload image; returns its load address.
fn load_payload() -> Result<u64, &'static str> {
    // SAFETY: the context contract makes the legacy shadow loadable RAM.
    unsafe { crate::directory::grant_load_window(LEGACY_SHADOW) }
        .map_err(|_| "cannot open the legacy BIOS shadow for loading")?;
    let context = ffs_context::memory_mapped().ok_or("no mounted boot media")?;
    let size = usize::try_from(context.image_size).map_err(|_| "boot media too large")?;
    // SAFETY: the mainstage published this mapped, verified firmware window.
    let media = unsafe {
        fstart_core::services::boot_media::MemoryMapped::from_raw_addr(context.image_base, size)
    };
    crate::load_ffs_file_entry_by_type(&media, FileType::Payload)
        .ok_or("payload load or verification failed")
}

fn halt(reason: &str) -> ! {
    fstart_log::error!("coreboot payload: {}", reason);
    fstart_arch::x86::boot::halt()
}
