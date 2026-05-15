//! fstart-stage: the single firmware stage binary crate.
//!
//! This crate's entire behavior is generated at build time from the board
//! RON file via `build.rs` + `fstart-codegen`. The generated code provides
//! `fstart_main()` with platform entry, driver init, and capability sequence.
//!
//! To build for a specific board:
//!   FSTART_BOARD_RON=boards/qemu-riscv64/board.ron \
//!     cargo build -p fstart-stage --target riscv64gc-unknown-none-elf \
//!     --features riscv64,ns16550 -Z build-std=core

#![no_std]
#![no_main]

// When a feature requiring heap allocation is active, pull in fstart-alloc
// to register the global allocator.  Without this explicit extern crate,
// the linker would not include it (nothing else references the crate by
// symbol).
#[cfg(any(
    feature = "acpi",
    feature = "pci-ecam",
    feature = "q35-hostbridge",
    feature = "crabefi"
))]
extern crate fstart_alloc;

/// Trampoline used by x86 bootblocks to run FFS stage loading on a DRAM
/// stack after raminit. The bootblock's normal stack is still Cache-as-RAM
/// and is too small for manifest parsing.
#[cfg(target_arch = "x86_64")]
#[no_mangle]
pub extern "C" fn __fstart_stage_load_mmio_trampoline(
    next_ptr: *const u8,
    next_len: usize,
    anchor_ptr: *const u8,
    anchor_len: usize,
    base: u64,
    size: u64,
) -> ! {
    // SAFETY: generated code passes pointers derived from valid `&str` and
    // anchor slices with their original lengths.
    let next_stage =
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(next_ptr, next_len)) };
    // SAFETY: generated code passes the embedded anchor slice pointer/length.
    let anchor = unsafe { core::slice::from_raw_parts(anchor_ptr, anchor_len) };
    raw_com1("\r\npostcar: teardown\r\n");
    // SAFETY: the generated trampoline has switched to a DRAM stack and will
    // never return to CAR-backed state.
    unsafe {
        fstart_platform::car_teardown::car_teardown();
        fstart_platform::car_teardown::postcar_mtrr_setup();
    }
    raw_com1("postcar: load ramstage\r\n");

    let entry = __fstart_quiet_stage_load(next_stage, anchor, base, size);
    raw_com1("postcar: jump ramstage\r\n");
    fstart_platform::jump_to(entry)
}

#[cfg(target_arch = "x86_64")]
fn raw_com1(s: &str) {
    for &b in s.as_bytes() {
        // SAFETY: COM1 is initialized by the bootblock console path.
        unsafe { fstart_pio::outb(0x3f8, b) };
    }
}

#[cfg(target_arch = "x86_64")]
fn __fstart_quiet_stage_load(next_stage: &str, anchor_data: &[u8], base: u64, size: u64) -> u64 {
    use fstart_types::ffs::{Compression, EntryContent, SegmentKind};

    let image = unsafe { core::slice::from_raw_parts(base as *const u8, size as usize) };
    let anchor = match unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_data) } {
        Ok(a) => a,
        Err(_) => loop {},
    };
    let image_size = (anchor.total_image_size as usize).min(size as usize);
    let reader = fstart_ffs::FfsReader::new(&image[..image_size]);
    let manifest = match reader.read_manifest(&anchor) {
        Ok(m) => m,
        Err(_) => loop {},
    };

    for region in &manifest.regions {
        let Ok(entry) = fstart_ffs::FfsReader::find_entry(region, next_stage) else {
            continue;
        };
        let EntryContent::File { segments, .. } = &entry.content else {
            continue;
        };

        let mut entry_addr = 0;
        for seg in segments {
            if entry_addr == 0 && seg.kind == SegmentKind::Code {
                entry_addr = seg.load_addr;
            }

            if seg.kind == SegmentKind::Bss {
                unsafe {
                    core::ptr::write_bytes(seg.load_addr as *mut u8, 0, seg.loaded_size as usize)
                };
                continue;
            }

            let src_off = (region.offset + entry.offset + seg.offset) as usize;
            let stored = seg.stored_size as usize;
            if src_off.saturating_add(stored) > image_size {
                loop {}
            }

            match seg.compression {
                Compression::None => unsafe {
                    core::ptr::copy(
                        image.as_ptr().add(src_off),
                        seg.load_addr as *mut u8,
                        stored,
                    );
                },
                #[cfg(feature = "lz4")]
                Compression::Lz4 => {
                    let buf_size = seg.in_place_size as usize;
                    let loaded = seg.loaded_size as usize;
                    if buf_size < loaded || buf_size < stored {
                        loop {}
                    }

                    let dest = seg.load_addr as *mut u8;
                    let comp_offset = buf_size - stored;
                    unsafe {
                        let buf = core::slice::from_raw_parts_mut(dest, buf_size);
                        core::ptr::copy(
                            image.as_ptr().add(src_off),
                            buf.as_mut_ptr().add(comp_offset),
                            stored,
                        );
                        let src =
                            core::slice::from_raw_parts(buf.as_ptr().add(comp_offset), stored);
                        let dst = core::slice::from_raw_parts_mut(buf.as_mut_ptr(), loaded);
                        match fstart_ffs::lz4::decompress_block(src, dst) {
                            Ok(_) => {}
                            Err(_) => loop {},
                        }
                    }
                }
                #[cfg(not(feature = "lz4"))]
                Compression::Lz4 => loop {},
            }
        }
        if entry_addr != 0 {
            return entry_addr;
        }
    }

    loop {}
}

// Include the generated stage code (fstart_main, driver instances, etc.)
include!(concat!(env!("OUT_DIR"), "/generated_stage.rs"));
