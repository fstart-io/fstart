//! SMRAM placement math for PIC SMM images.
//!
//! The layout follows the same hardware constraints coreboot handles in
//! `smm_module_loader.c`: each CPU enters at `SMBASE + 0x8000`, while its
//! save-state area lives at the top of the 64 KiB SMBASE window and grows
//! downward.  fstart differs by copying one of several precompiled PIC entry
//! stubs per CPU rather than loading one relocatable stub and duplicating it.

use crate::runtime::MAX_SMM_CPUS;

/// Architectural SMM entry offset from SMBASE.
pub const SMM_ENTRY_OFFSET: u64 = 0x8000;
/// Architectural default/per-CPU SMM window size.
pub const SMM_CODE_SEGMENT_SIZE: u64 = 0x1_0000;

/// Errors from SMRAM layout computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// `entry_count` is zero.
    NoEntries,
    /// The requested entry count exceeds the fixed ABI cap.
    TooManyEntries,
    /// A size argument is zero or otherwise unusable.
    BadSize,
    /// The entry stub would overlap the architectural entry offset or save state.
    StubDoesNotFit,
    /// The requested regions do not fit inside SMRAM.
    SmramTooSmall,
    /// Address arithmetic overflowed.
    Overflow,
}

/// Inputs for computing the permanent SMRAM layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmramLayout {
    /// Permanent SMRAM/TSEG base.
    pub smram_base: u64,
    /// Permanent SMRAM/TSEG size.
    pub smram_size: u64,
    /// Number of precompiled entry stubs / CPU slots to place.
    pub entry_count: u16,
    /// Size of each CPU save-state area.
    pub save_state_size: u32,
    /// Per-CPU SMM stack size.
    pub stack_size: u32,
    /// Maximum copied stub size.
    pub entry_stub_size: u32,
    /// Common handler/data blob size.
    pub common_size: u32,
    /// Optional page-table bytes below the handler/data region for long mode.
    pub page_table_size: u32,
}

/// Computed per-CPU SMM placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSmmLayout {
    /// SMBASE written into the CPU save state during relocation.
    pub smbase: u64,
    /// Address where the entry stub is copied (`smbase + 0x8000`).
    pub entry_addr: u64,
    /// Save-state area base.
    pub save_state_base: u64,
    /// Save-state area top (exclusive).
    pub save_state_top: u64,
    /// Stack bottom.
    pub stack_bottom: u64,
    /// Stack top (exclusive), selected by the entry stub for this CPU.
    pub stack_top: u64,
}

/// Compute the base address of the copied SMM handler/data region.
pub fn compute_common_base(layout: &SmramLayout) -> Result<u64, LayoutError> {
    if layout.smram_size == 0 {
        return Err(LayoutError::BadSize);
    }
    let smram_top = layout
        .smram_base
        .checked_add(layout.smram_size)
        .ok_or(LayoutError::Overflow)?;
    let top_reserved = align_up(layout.common_size as u64, 16)?
        .checked_add(align_up(layout.page_table_size as u64, 4096)?)
        .ok_or(LayoutError::Overflow)?;
    smram_top
        .checked_sub(top_reserved)
        .ok_or(LayoutError::SmramTooSmall)
}

/// Compute the per-CPU SMM layout.
///
/// `out` must have room for at least `layout.entry_count` entries.  The return
/// value is the populated prefix of `out`.
pub fn compute_cpu_layout<'a>(
    layout: &SmramLayout,
    out: &'a mut [CpuSmmLayout],
) -> Result<&'a [CpuSmmLayout], LayoutError> {
    let count = layout.entry_count as usize;
    if count == 0 {
        return Err(LayoutError::NoEntries);
    }
    if count > MAX_SMM_CPUS || count > out.len() {
        return Err(LayoutError::TooManyEntries);
    }
    if layout.smram_size == 0
        || layout.save_state_size == 0
        || layout.stack_size == 0
        || layout.entry_stub_size == 0
    {
        return Err(LayoutError::BadSize);
    }
    if layout.entry_stub_size as u64 >= SMM_ENTRY_OFFSET {
        return Err(LayoutError::StubDoesNotFit);
    }

    // Common handler and optional page tables are placed at the top of SMRAM.
    let common_base = compute_common_base(layout)?;

    // Stacks grow upward from the beginning of SMRAM as a contiguous region.
    let total_stack = (layout.stack_size as u64)
        .checked_mul(layout.entry_count as u64)
        .ok_or(LayoutError::Overflow)?;
    let stacks_end = layout
        .smram_base
        .checked_add(total_stack)
        .ok_or(LayoutError::Overflow)?;
    if stacks_end > common_base {
        return Err(LayoutError::SmramTooSmall);
    }

    let needed_ss_size =
        core::cmp::max(layout.save_state_size as u64, layout.entry_stub_size as u64);
    let per_segment =
        (SMM_CODE_SEGMENT_SIZE - SMM_ENTRY_OFFSET - layout.entry_stub_size as u64) / needed_ss_size;
    if per_segment == 0 {
        return Err(LayoutError::StubDoesNotFit);
    }

    // First segment begins immediately below the top-reserved common/page-table
    // area, then more 64 KiB windows are allocated downward as needed.
    let first_segment_base = common_base
        .checked_sub(SMM_CODE_SEGMENT_SIZE)
        .ok_or(LayoutError::SmramTooSmall)?;

    for (i, slot) in out.iter_mut().take(count).enumerate() {
        let segment = (i as u64) / per_segment;
        let in_segment = (i as u64) % per_segment;
        let smbase = first_segment_base
            .checked_sub(
                segment
                    .checked_mul(SMM_CODE_SEGMENT_SIZE)
                    .ok_or(LayoutError::Overflow)?,
            )
            .and_then(|v| v.checked_sub(in_segment.checked_mul(needed_ss_size)?))
            .ok_or(LayoutError::SmramTooSmall)?;
        let entry_addr = smbase
            .checked_add(SMM_ENTRY_OFFSET)
            .ok_or(LayoutError::Overflow)?;
        let save_state_top = smbase
            .checked_add(SMM_CODE_SEGMENT_SIZE)
            .ok_or(LayoutError::Overflow)?;
        let save_state_base = save_state_top
            .checked_sub(layout.save_state_size as u64)
            .ok_or(LayoutError::Overflow)?;
        let stack_bottom = layout
            .smram_base
            .checked_add((i as u64) * layout.stack_size as u64)
            .ok_or(LayoutError::Overflow)?;
        let stack_top = stack_bottom
            .checked_add(layout.stack_size as u64)
            .ok_or(LayoutError::Overflow)?;

        if smbase < stacks_end || save_state_top > common_base {
            return Err(LayoutError::SmramTooSmall);
        }
        if entry_addr + layout.entry_stub_size as u64 > save_state_base {
            return Err(LayoutError::StubDoesNotFit);
        }

        *slot = CpuSmmLayout {
            smbase,
            entry_addr,
            save_state_base,
            save_state_top,
            stack_bottom,
            stack_top,
        };
    }

    Ok(&out[..count])
}

fn align_up(value: u64, align: u64) -> Result<u64, LayoutError> {
    debug_assert!(align.is_power_of_two());
    value
        .checked_add(align - 1)
        .map(|v| v & !(align - 1))
        .ok_or(LayoutError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_four_q35_entries() {
        let layout = SmramLayout {
            smram_base: 0x7f00_0000,
            smram_size: 0x80_0000,
            entry_count: 4,
            save_state_size: 0x400,
            stack_size: 0x400,
            entry_stub_size: 0x600,
            common_size: 0x4000,
            page_table_size: 0x3000,
        };
        let mut cpus = [CpuSmmLayout {
            smbase: 0,
            entry_addr: 0,
            save_state_base: 0,
            save_state_top: 0,
            stack_bottom: 0,
            stack_top: 0,
        }; 4];
        let out = compute_cpu_layout(&layout, &mut cpus).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].entry_addr, out[0].smbase + SMM_ENTRY_OFFSET);
        assert!(out[0].save_state_base > out[0].entry_addr);
        assert!(out[3].smbase < layout.smram_base + layout.smram_size);
    }

    #[test]
    fn rejects_oversized_stub() {
        let layout = SmramLayout {
            smram_base: 0,
            smram_size: 0x1_0000,
            entry_count: 1,
            save_state_size: 0x400,
            stack_size: 0x400,
            entry_stub_size: 0x8000,
            common_size: 0,
            page_table_size: 0,
        };
        let mut cpus = [CpuSmmLayout {
            smbase: 0,
            entry_addr: 0,
            save_state_base: 0,
            save_state_top: 0,
            stack_bottom: 0,
            stack_top: 0,
        }; 1];
        assert_eq!(
            compute_cpu_layout(&layout, &mut cpus).unwrap_err(),
            LayoutError::StubDoesNotFit
        );
    }
}
