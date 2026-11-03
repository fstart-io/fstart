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
    not(any(target_arch = "x86", target_arch = "x86_64"))
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

/// Kept only by [`pinned_bootstrap_descriptor`], which only the initial
/// stage calls: packaging finds exactly one placeholder.
#[cfg(all(
    feature = "bootstrap",
    not(any(target_arch = "x86", target_arch = "x86_64"))
))]
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
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
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
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        Err(fstart_ffs::root::RootError::UnsupportedVersion)
    }
}

/// Write the handoff for the next stage to a DRAM buffer: the trained DRAM
/// size and this stage's image locator.
///
/// # Safety
///
/// `handoff_addr` must point to writable RAM with at least
/// [`HANDOFF_MAX_SIZE`](fstart_core::handoff::HANDOFF_MAX_SIZE) bytes, reserved
/// below the next stage's load address.
///
/// # Errors
///
/// Returns `Err` when no locator is published.
#[cfg(feature = "handoff")]
pub unsafe fn write_handoff(dram_size: u64, handoff_addr: u64) -> Result<(), &'static str> {
    let locator = crate::anchor::locator().ok_or("initial locator missing")?;
    let handoff = fstart_core::handoff::StageHandoff::new(dram_size, locator);
    // SAFETY: the caller reserves this writable range.
    let buf = unsafe {
        core::slice::from_raw_parts_mut(
            handoff_addr as *mut u8,
            fstart_core::handoff::HANDOFF_MAX_SIZE,
        )
    };
    handoff.write_to(buf).ok_or("handoff buffer too small")?;
    Ok(())
}
