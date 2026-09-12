//! Pinned bootstrap metadata and fixed-family handoff serialization.
//! Executable media reads go through `boot::load_bootstrap`; there is no
//! unchecked direct-to-address stage copy helper.

/// Build-patched bootstrap descriptor in the initial stage. Updating a pin
/// requires replacing that initial image; without protection of that image this
/// is development integrity, not authenticated updates.
#[cfg(feature = "bootstrap")]
#[repr(C, align(8))]
pub struct BootstrapPin(core::cell::UnsafeCell<[u8; 160]>);

#[cfg(feature = "bootstrap")]
unsafe impl Sync for BootstrapPin {}

#[cfg(all(
    feature = "bootstrap",
    fstart_stage_env = "car",
    not(target_arch = "x86_64")
))]
const fn bootstrap_pin_placeholder() -> [u8; 160] {
    let mut bytes = [0; 160];
    let magic = *b"FSTPIN01";
    let mut index = 0;
    while index < magic.len() {
        bytes[index] = magic[index];
        index += 1;
    }
    bytes
}

#[cfg(all(
    feature = "bootstrap",
    fstart_stage_env = "car",
    not(target_arch = "x86_64")
))]
#[used]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.bootstrap_pin"))]
pub static FSTART_BOOTSTRAP_PIN: BootstrapPin =
    BootstrapPin(core::cell::UnsafeCell::new(bootstrap_pin_placeholder()));

/// Read the descriptor pinned by image construction into this initial stage.
/// A still-unpatched marker fails descriptor parsing. The copy is bounded and
/// remains stable even when the initial image is executing from mapped flash.
#[cfg(feature = "bootstrap")]
pub fn pinned_bootstrap_descriptor()
-> Result<fstart_ffs::root::BootstrapDescriptor, fstart_ffs::root::RootError> {
    #[cfg(all(fstart_stage_env = "car", not(target_arch = "x86_64")))]
    {
        let mut bytes = [0; 160];
        for (index, byte) in bytes.iter_mut().enumerate() {
            // SAFETY: the static contains 160 initialized bytes, patched offline;
            // volatile prevents constant folding of the link-time placeholder.
            *byte = unsafe {
                core::ptr::read_volatile(FSTART_BOOTSTRAP_PIN.0.get().cast::<u8>().add(index))
            };
        }
        fstart_ffs::root::BootstrapDescriptor::parse(&bytes)
    }
    #[cfg(not(all(fstart_stage_env = "car", not(target_arch = "x86_64"))))]
    {
        Err(fstart_ffs::root::RootError::UnsupportedVersion)
    }
}

/// Serialize handoff data to a DRAM buffer for the next stage.
///
/// Writes a [`StageHandoff`](fstart_core::handoff::StageHandoff) to
/// `handoff_addr` and returns the number of bytes written.
///
/// # Safety
///
/// Caller must ensure `handoff_addr` points to writable RAM with at
/// least [`HANDOFF_MAX_SIZE`](fstart_core::handoff::HANDOFF_MAX_SIZE)
/// bytes. This is guaranteed by placing the handoff buffer at a known
/// offset below the next stage's load address.
///
/// # Errors
///
/// Returns `Err` if postcard serialization fails (buffer too small or
/// encoding error).
#[cfg(feature = "handoff")]
pub fn serialize_handoff(dram_size: u64, handoff_addr: u64) -> Result<usize, &'static str> {
    let mut handoff_data = fstart_core::handoff::StageHandoff::new(dram_size);
    handoff_data.media = Some(crate::anchor::media_locator().ok_or("initial locator missing")?);
    // SAFETY: handoff_addr points to writable RAM, 4K below next stage load_addr.
    let handoff_buf = unsafe {
        core::slice::from_raw_parts_mut(
            handoff_addr as *mut u8,
            fstart_core::handoff::HANDOFF_MAX_SIZE,
        )
    };
    let handoff_len = crate::handoff::serialize(&handoff_data, handoff_buf)?;
    fstart_log::info!("handoff: {} bytes at {:#x}", handoff_len, handoff_addr);
    Ok(handoff_len)
}
