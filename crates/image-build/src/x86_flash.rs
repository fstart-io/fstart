//! An x86 flash image mapped below 4 GiB, as QEMU's q35 maps its
//! `-bios` image: the FFS at its offset in the flash and the linked
//! stage's file-backed flash segments (bootblock, reset vector) at their
//! load addresses, with the stage's anchor and trust copies patched from
//! the FFS it carries.
use crate::build_plan::BuildPlan;
use fstart_core::ffs::trust::{TRUST_SIZE, TrustBlock};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The x86 flash image operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct X86FlashAssembly {
    /// The FFS image step whose artefact the flash carries.
    pub ffs: String,
    /// The stage unit whose linked ELF supplies the flash segments.
    pub stage_unit: String,
    /// Flash size; the flash ends at 4 GiB.
    pub size: u64,
    /// Where the FFS sits in the flash.
    pub ffs_offset: u64,
}

impl X86FlashAssembly {
    pub fn validate(&self, plan: &BuildPlan) -> Result<(), String> {
        if !plan.platform.is_x86() {
            return Err("an x86 flash image needs an x86 plan".into());
        }
        if !plan.stages.contains(&self.stage_unit) {
            return Err(format!(
                "x86 flash stage unit {} is no image stage",
                self.stage_unit
            ));
        }
        if self.size == 0 || self.size > 1 << 32 || self.ffs_offset >= self.size {
            return Err("x86 flash size or FFS offset out of range".into());
        }
        Ok(())
    }

    /// The flash image of `ffs` and the linked `stage_elf`. Returns the
    /// image and what was patched where.
    pub fn compose(
        &self,
        ffs: &[u8],
        stage_elf: &[u8],
        stage_path: &Path,
    ) -> Result<(Vec<u8>, Vec<String>), String> {
        let size = usize::try_from(self.size).map_err(|_| "x86 flash size exceeds memory")?;
        let flash_base = (1u64 << 32) - self.size;
        let mut flash = vec![0xffu8; size];
        let mut notes = Vec::new();
        overlay_elf_segments(stage_elf, stage_path, &mut flash, flash_base)?;
        notes.push(format!(
            "x86 flash: overlaid flash PT_LOAD segments from {}",
            stage_path.display()
        ));

        let ffs_offset = self.ffs_offset as usize;
        let ffs_end = ffs_offset
            .checked_add(ffs.len())
            .ok_or("FFS range overflows the x86 flash")?;
        let bootblock_start = size.saturating_sub(0x1000); // last 4K
        if ffs_end > bootblock_start {
            return Err(format!(
                "FFS image ({} bytes) at offset {ffs_offset:#x} overlaps boot block at {bootblock_start:#x}",
                ffs.len(),
            ));
        }
        flash[ffs_offset..ffs_end].copy_from_slice(ffs);

        let ffs_anchor = ffs_anchor_offset(ffs)
            .ok_or("failed to locate the patched FSTART anchor in the FFS image")?;
        let stage_anchor =
            elf_symbol_flash_offset(stage_elf, "_fstart_anchor_early", flash_base, self.size)?
                .ok_or_else(|| {
                    format!(
                        "missing _fstart_anchor_early in {} while building the x86 flash",
                        stage_path.display()
                    )
                })?;
        let anchor_size = fstart_core::ffs::ANCHOR_SIZE;
        if ffs_anchor + anchor_size > ffs.len() || stage_anchor + anchor_size > flash.len() {
            return Err(format!(
                "x86 anchor patch out of bounds: FFS offset {ffs_anchor:#x}, flash offset {stage_anchor:#x}"
            ));
        }
        flash[stage_anchor..stage_anchor + anchor_size]
            .copy_from_slice(&ffs[ffs_anchor..ffs_anchor + anchor_size]);
        notes.push(format!(
            "x86 flash: patched anchor at flash offset {stage_anchor:#x} (from FFS offset {ffs_anchor:#x})"
        ));
        // The trust search must see final bytes, and the patched site must
        // not overlap the FFS window it was just copied beside.
        let trust = patch_stage_trust(&mut flash, ffs)?;
        if trust < ffs_end && trust + TRUST_SIZE > ffs_offset {
            return Err(format!(
                "stage trust at flash offset {trust:#x} overlaps the FFS window \
                 [{ffs_offset:#x}..{ffs_end:#x})",
            ));
        }
        notes.push(format!(
            "x86 flash: patched trust at flash offset {trust:#x}"
        ));
        Ok((flash, notes))
    }
}

/// The offset of the FFS's own anchor: the `FSTART01` block whose total
/// image size and anchor offset fields describe this image.
fn ffs_anchor_offset(ffs: &[u8]) -> Option<usize> {
    let field = |offset: usize| {
        u32::from_le_bytes(ffs[offset..offset + 4].try_into().expect("4 bytes")) as usize
    };
    ffs.windows(fstart_core::ffs::FFS_MAGIC.len())
        .enumerate()
        .find_map(|(offset, window)| {
            (window == fstart_core::ffs::FFS_MAGIC
                && offset + 28 <= ffs.len()
                && field(offset + 20) == ffs.len()
                && field(offset + 24) == offset)
                .then_some(offset)
        })
}

fn elf_symbol_flash_offset(
    elf: &[u8],
    symbol: &str,
    flash_base: u64,
    flash_size: u64,
) -> Result<Option<usize>, String> {
    use object::{Object, ObjectSymbol};
    let file = object::File::parse(elf).map_err(|e| format!("failed to parse stage ELF: {e}"))?;
    Ok(file.symbols().find_map(|sym| {
        let address = sym.address();
        (sym.name() == Ok(symbol) && (flash_base..flash_base + flash_size).contains(&address))
            .then(|| (address - flash_base) as usize)
    }))
}

/// Overlay the file-backed `PT_LOAD` segments of `elf` that lie in the
/// flash at their physical addresses. A zero overlay means a mis-linked
/// ELF or a wrong base and would silently produce a flash without reset
/// vector, so it is an error.
fn overlay_elf_segments(
    elf: &[u8],
    path: &Path,
    flash: &mut [u8],
    flash_base: u64,
) -> Result<(), String> {
    let flash_end = flash_base + flash.len() as u64;
    let mut overlaid = 0u32;
    for segment in crate::assemble::elf_load_segments(elf, path)? {
        if segment.filesz == 0 {
            continue;
        }
        let end = segment
            .paddr
            .checked_add(segment.filesz)
            .ok_or("stage ELF segment address overflow")?;
        if segment.paddr < flash_base || end > flash_end {
            continue;
        }
        let source = usize::try_from(segment.offset)
            .ok()
            .zip(usize::try_from(segment.filesz).ok())
            .and_then(|(start, size)| elf.get(start..start.checked_add(size)?))
            .ok_or_else(|| {
                format!(
                    "stage ELF segment exceeds file size: offset {:#x}, size {:#x}",
                    segment.offset, segment.filesz
                )
            })?;
        let destination = (segment.paddr - flash_base) as usize;
        flash[destination..destination + source.len()].copy_from_slice(source);
        overlaid += 1;
    }
    if overlaid == 0 {
        return Err(format!(
            "no flash PT_LOAD segments overlaid from {}",
            path.display()
        ));
    }
    Ok(())
}

/// Copy the finalized FFS trust policy over the stage's placeholder trust
/// block. The ELF overlay carries linked placeholder bytes, but runtime
/// reads its expected keys from linked code, so the ROM copy must carry
/// the policy the image builder finalized. Returns the patched offset.
fn patch_stage_trust(flash: &mut [u8], ffs: &[u8]) -> Result<usize, String> {
    let mut placeholder = [0u8; TRUST_SIZE];
    TrustBlock::placeholder().write_to(&mut placeholder);
    let trust = (0..ffs.len().saturating_sub(TRUST_SIZE - 1))
        .step_by(8)
        .filter_map(|offset| {
            let window = &ffs[offset..offset + TRUST_SIZE];
            if window == placeholder {
                return None;
            }
            TrustBlock::parse(window).filter(|block| block.key_count > 0)
        })
        .next()
        .ok_or("no finalized trust block found in the FFS image")?;
    let mut patched = [0u8; TRUST_SIZE];
    trust.write_to(&mut patched);
    let destination = (0..flash.len().saturating_sub(TRUST_SIZE - 1))
        .step_by(8)
        .find(|&offset| flash[offset..offset + TRUST_SIZE] == placeholder)
        .ok_or("stage trust placeholder not found in the x86 flash")?;
    flash[destination..destination + TRUST_SIZE].copy_from_slice(&patched);
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_uses_the_physical_load_address() {
        let flash_base = 0xff00_0000u64;
        let paddr = flash_base + 0x20;
        // One PT_LOAD whose RAM vaddr differs from its flash paddr.
        let elf64 = {
            let mut elf = vec![0u8; 0x104];
            elf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
            elf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
            elf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
            let ph = 64;
            elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
            elf[ph + 8..ph + 16].copy_from_slice(&0x100u64.to_le_bytes());
            elf[ph + 16..ph + 24].copy_from_slice(&0x0100_0000u64.to_le_bytes());
            elf[ph + 24..ph + 32].copy_from_slice(&paddr.to_le_bytes());
            elf[ph + 32..ph + 40].copy_from_slice(&4u64.to_le_bytes());
            (2, elf)
        };
        let elf32 = {
            let mut elf = vec![0u8; 0x104];
            elf[28..32].copy_from_slice(&52u32.to_le_bytes()); // e_phoff
            elf[42..44].copy_from_slice(&32u16.to_le_bytes()); // e_phentsize
            elf[44..46].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
            let ph = 52;
            elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
            elf[ph + 4..ph + 8].copy_from_slice(&0x100u32.to_le_bytes());
            elf[ph + 8..ph + 12].copy_from_slice(&0x0100_0000u32.to_le_bytes());
            elf[ph + 12..ph + 16].copy_from_slice(&(paddr as u32).to_le_bytes());
            elf[ph + 16..ph + 20].copy_from_slice(&4u32.to_le_bytes());
            (1, elf)
        };
        for (class, mut elf) in [elf64, elf32] {
            elf[0..4].copy_from_slice(b"\x7fELF");
            elf[4] = class;
            elf[5] = 1; // little-endian
            elf[6] = 1; // EV_CURRENT
            elf[0x100..0x104].copy_from_slice(b"data");
            let mut flash = vec![0xff; 0x100];
            overlay_elf_segments(&elf, Path::new("stage.elf"), &mut flash, flash_base).unwrap();
            assert_eq!(&flash[0x20..0x24], b"data");
        }
    }
}
