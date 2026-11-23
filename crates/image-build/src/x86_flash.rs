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
        let anchor_size = fstart_core::ffs::LOCATOR_SIZE;
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

/// An x86 RO/A or RO/A/B flash image mapped below 4 GiB:
/// - top-aligned RO stage ELF segments confined to `[ro_offset..size)` (with symbol-patched `FSTART_TRUST`),
/// - `SLOT_STATE_0` / `SLOT_STATE_1` journal initialized to `Slot::A`,
/// - `RW_A` (and `RW_B` in RO/A/B) populated from the authenticated RW-slot FFS artefact,
/// - optional seeded NVRAM variable store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct X86AbFlashAssembly {
    /// The RW-slot FFS image step whose artefact populates `RW_A` (and `RW_B`).
    pub ffs: String,
    /// The RO stage unit whose linked ELF supplies the top-aligned x86 reset-vector segments.
    pub ro_stage_unit: String,
    /// Flash size in bytes; the flash ends at 4 GiB.
    pub size: u64,
    /// Flash offset where the read-only RO region begins; all RW slots,
    /// journal sectors, and NVRAM must lie strictly below `ro_offset`, and all
    /// RO ELF flash `PT_LOAD` segments must lie in `[ro_offset..size)`.
    pub ro_offset: u32,
    /// RO/A or RO/A/B slot layout.
    pub slot_layout: crate::capsule::AbSlotLayout,
    /// Optional NVRAM variable store `(flash_offset, size)` for seeded settings/variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvram: Option<(u32, u32)>,
}

impl X86AbFlashAssembly {
    fn reserved_ranges(&self) -> Result<Vec<(usize, usize)>, String> {
        use fstart_ffs::slot_state::{SECTOR_SIZE, Slot};
        let cap = u32::try_from(self.size).map_err(|_| "x86 A/B flash size exceeds u32")?;
        if self.ro_offset == 0
            || self.ro_offset >= cap
            || !(self.ro_offset as usize).is_multiple_of(SECTOR_SIZE)
        {
            return Err(
                "x86 A/B flash ro_offset must be 4 KiB aligned and strictly inside flash".into(),
            );
        }
        let layout = self.slot_layout.to_slot_layout();
        layout
            .validate(self.ro_offset)
            .map_err(|e| format!("x86 A/B flash slot layout invalid or overlaps RO: {e:?}"))?;
        let (s0, s1) = layout.state_sectors();
        let mut ranges = vec![
            (s0 as usize, s0 as usize + SECTOR_SIZE),
            (s1 as usize, s1 as usize + SECTOR_SIZE),
        ];
        for slot in [Slot::A, Slot::B] {
            if let Some(ext) = layout.extent(slot) {
                ranges.push((ext.offset as usize, (ext.offset + ext.size) as usize));
            }
        }
        if let Some((off, sz)) = self.nvram {
            if sz == 0
                || !(off as usize).is_multiple_of(SECTOR_SIZE)
                || !(sz as usize).is_multiple_of(SECTOR_SIZE)
            {
                return Err(
                    "x86 A/B flash NVRAM extent must be non-empty and 4 KiB aligned".into(),
                );
            }
            let end = off
                .checked_add(sz)
                .filter(|&e| e <= self.ro_offset)
                .ok_or("x86 A/B flash NVRAM extent exceeds RW bank or overlaps RO")?;
            let nr = (off as usize, end as usize);
            if ranges.iter().any(|&(a, b)| nr.0 < b && a < nr.1) {
                return Err("x86 A/B flash NVRAM extent overlaps slot layout".into());
            }
            ranges.push(nr);
        }
        Ok(ranges)
    }

    pub fn validate(&self, plan: &BuildPlan) -> Result<(), String> {
        if !plan.platform.is_x86() {
            return Err("an x86 A/B flash image needs an x86 plan".into());
        }
        if !plan.stages.contains(&self.ro_stage_unit) {
            return Err(format!(
                "x86 A/B flash RO stage unit {} is no image stage",
                self.ro_stage_unit
            ));
        }
        if self.size == 0 || self.size > (1u64 << 32) {
            return Err("x86 A/B flash size out of range".into());
        }
        self.reserved_ranges()?;
        Ok(())
    }

    /// Compose the full RO/A(/B) flash image from the RW-slot FFS image,
    /// deterministic [`TrustBlock`] bytes, and the linked RO stage ELF.
    pub fn compose(
        &self,
        rw_ffs: &[u8],
        trust_bytes: &[u8; TRUST_SIZE],
        ro_elf: &[u8],
        ro_path: &Path,
        nvram_seed: Option<&[u8]>,
    ) -> Result<(Vec<u8>, Vec<String>), String> {
        let reserved = self.reserved_ranges()?;
        let size = usize::try_from(self.size).map_err(|_| "x86 A/B flash size exceeds memory")?;
        let flash_base = (1u64 << 32) - self.size;
        let ro_min_offset = self.ro_offset as usize;
        let mut flash = vec![0xffu8; size];
        let mut notes = Vec::new();

        audit_ro_ramcode(ro_elf, ro_path, flash_base, self.size)?;

        overlay_elf_segments_checked(
            ro_elf,
            ro_path,
            &mut flash,
            flash_base,
            ro_min_offset,
            &reserved,
        )?;
        notes.push(format!(
            "x86 A/B flash: overlaid RO flash PT_LOAD segments from {} (confined to >= {ro_min_offset:#x})",
            ro_path.display()
        ));

        // Patch FSTART_TRUST deterministically by ELF symbol offset, verifying
        // it sits inside the RO bank and currently holds the link-time placeholder.
        let trust = TrustBlock::parse(trust_bytes)
            .filter(|t| t.key_count.get() > 0)
            .ok_or("x86 A/B flash: invalid or empty TrustBlock from FFS step")?;
        let trust_off =
            elf_trust_flash_offset(ro_elf, flash_base, self.size)?.ok_or_else(|| {
                format!(
                    "missing FSTART_TRUST symbol in {} while building x86 A/B flash",
                    ro_path.display()
                )
            })?;
        if trust_off < ro_min_offset || trust_off + TRUST_SIZE > flash.len() {
            return Err(format!(
                "RO stage FSTART_TRUST at flash offset {trust_off:#x} falls outside RO region [{ro_min_offset:#x}..{:#x})",
                flash.len()
            ));
        }
        let mut placeholder = [0u8; TRUST_SIZE];
        TrustBlock::placeholder().write_to(&mut placeholder);
        if flash[trust_off..trust_off + TRUST_SIZE] != placeholder {
            return Err(format!(
                "RO stage FSTART_TRUST at flash offset {trust_off:#x} does not match expected placeholder"
            ));
        }
        flash[trust_off..trust_off + TRUST_SIZE].copy_from_slice(trust_bytes);
        notes.push(format!(
            "x86 A/B flash: patched FSTART_TRUST at flash offset {trust_off:#x}"
        ));

        // Authenticate the RW slot's signed Root against the exact TrustBlock
        // we just patched into RO before populating the slots.
        if rw_ffs.len() < fstart_ffs::root::ROOT_SIZE {
            return Err("x86 A/B flash: RW FFS image smaller than Root".into());
        }
        let root_policy = fstart_ffs::root::RootPolicy {
            image_family: trust.image_family,
            minimum_security_version: trust.minimum_security_version.get(),
            image_size: rw_ffs.len() as u64,
            max_directory_size: 64 * 1024,
            keys: trust.valid_keys(),
        };
        fstart_ffs::root::authenticate_root(&root_policy, &rw_ffs[..fstart_ffs::root::ROOT_SIZE])
            .map_err(|e| format!("x86 A/B flash: RW slot Root failed RO TrustBlock auth: {e:?}"))?;

        let layout = self.slot_layout.to_slot_layout();
        crate::capsule::populate_rw_slots(&mut flash, layout, rw_ffs)?;
        notes.push(format!(
            "x86 A/B flash: populated RW slot(s) ({} bytes) and SLOT_STATE journal",
            rw_ffs.len()
        ));

        if let Some(seed) = nvram_seed {
            let Some((off, sz)) = self.nvram else {
                return Err("x86 A/B flash has no NVRAM extent for seeded settings".into());
            };
            if seed.len() != sz as usize {
                return Err(format!(
                    "x86 A/B flash NVRAM seed ({} bytes) does not match extent ({sz} bytes)",
                    seed.len()
                ));
            }
            let start = off as usize;
            flash[start..start + seed.len()].copy_from_slice(seed);
            notes.push(format!(
                "x86 A/B flash: seeded NVRAM ({:#x} bytes at {off:#x})",
                seed.len()
            ));
        }

        Ok((flash, notes))
    }
}

/// The offset of the FFS's own anchor: the `FSTART01` block whose total
/// image size and anchor offset fields describe this image.
fn ffs_anchor_offset(ffs: &[u8]) -> Option<usize> {
    let field = |offset: usize| {
        u32::from_le_bytes(ffs[offset..offset + 4].try_into().expect("4 bytes")) as usize
    };
    ffs.windows(fstart_core::ffs::LOCATOR_MAGIC.len())
        .enumerate()
        .find_map(|(offset, window)| {
            (window == fstart_core::ffs::LOCATOR_MAGIC
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

fn is_fstart_trust_symbol(name: &str) -> bool {
    name == "FSTART_TRUST"
        || name.ends_with("12fstart_stage6anchor12FSTART_TRUST")
        || (name.contains("12fstart_stage6anchor12FSTART_TRUST") && name.ends_with('E'))
}

fn elf_trust_flash_offset(
    elf: &[u8],
    flash_base: u64,
    flash_size: u64,
) -> Result<Option<usize>, String> {
    use object::{Object, ObjectSymbol};
    let file = object::File::parse(elf).map_err(|e| format!("failed to parse stage ELF: {e}"))?;
    let mut found: Option<usize> = None;
    for sym in file.symbols() {
        let Ok(name) = sym.name() else {
            continue;
        };
        if !is_fstart_trust_symbol(name) {
            continue;
        }
        let address = sym.address();
        if !(flash_base..flash_base + flash_size).contains(&address) {
            continue;
        }
        let size = sym.size() as usize;
        if size != 0 && size != TRUST_SIZE {
            return Err(format!(
                "FSTART_TRUST symbol `{name}` has size {size}, expected {TRUST_SIZE}"
            ));
        }
        let off = (address - flash_base) as usize;
        if found.is_some() {
            return Err("ambiguous FSTART_TRUST symbols in RO stage ELF".into());
        }
        found = Some(off);
    }
    Ok(found)
}

/// Verify that any `.ramcode` section in an RO stage ELF is linked into RAM
/// (VMA below `flash_base`) and, on host builds with `iced_x86`, contains no
/// control-flow transfers back into the flash window (`[flash_base..flash_end)`).
fn audit_ro_ramcode(
    elf: &[u8],
    path: &Path,
    flash_base: u64,
    flash_size: u64,
) -> Result<(), String> {
    use object::{Object, ObjectSection};
    let file = object::File::parse(elf).map_err(|e| format!("failed to parse RO ELF: {e}"))?;
    let Some(section) = file.section_by_name(".ramcode") else {
        return Ok(());
    };
    let data = section
        .data()
        .map_err(|e| format!("failed to read .ramcode in {}: {e}", path.display()))?;
    if data.is_empty() {
        return Ok(());
    }
    let vma = section.address();
    let vma_end = vma
        .checked_add(data.len() as u64)
        .ok_or("RO .ramcode VMA overflow")?;
    let flash_end = flash_base + flash_size;
    if vma >= flash_base || vma_end > flash_base {
        return Err(format!(
            "RO .ramcode in {} is linked at {vma:#x}..{vma_end:#x}, which is not in RAM below flash {flash_base:#x}",
            path.display()
        ));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind};
        let mut decoder = Decoder::with_ip(64, data, vma, DecoderOptions::NONE);
        while decoder.can_decode() {
            let instr = decoder.decode();
            if instr.is_invalid() {
                return Err(format!(
                    "RO .ramcode in {} contains undecodable x86_64 instruction at {:#x}",
                    path.display(),
                    instr.ip()
                ));
            }
            if instr.is_ip_rel_memory_operand() {
                let target = instr.ip_rel_memory_address();
                if (flash_base..flash_end).contains(&target) {
                    return Err(format!(
                        "RO .ramcode in {} accesses flash data at {target:#x} from {:#x}",
                        path.display(),
                        instr.ip()
                    ));
                }
            }
            if instr.op_count() > 0 {
                match instr.op0_kind() {
                    OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                        let target = instr.near_branch_target();
                        if !(vma..vma_end).contains(&target) {
                            return Err(format!(
                                "RO .ramcode in {} transfers control from {:#x} outside .ramcode to {target:#x}",
                                path.display(),
                                instr.ip()
                            ));
                        }
                    }
                    OpKind::FarBranch16 | OpKind::FarBranch32 => {
                        return Err(format!(
                            "RO .ramcode in {} uses a far branch/call at {:#x}",
                            path.display(),
                            instr.ip()
                        ));
                    }
                    OpKind::Register | OpKind::Memory
                        if matches!(instr.mnemonic(), Mnemonic::Call | Mnemonic::Jmp) =>
                    {
                        return Err(format!(
                            "RO .ramcode in {} uses an indirect branch/call at {:#x} (forbidden while flash may be in command mode)",
                            path.display(),
                            instr.ip()
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
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
    overlay_elf_segments_checked(elf, path, flash, flash_base, 0, &[])
}

fn overlay_elf_segments_checked(
    elf: &[u8],
    path: &Path,
    flash: &mut [u8],
    flash_base: u64,
    min_offset: usize,
    reserved: &[(usize, usize)],
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
        let dst_end = destination + source.len();
        if destination < min_offset {
            return Err(format!(
                "RO ELF segment [{destination:#x}..{dst_end:#x}) in {} escapes below ro_offset {min_offset:#x}",
                path.display()
            ));
        }
        if reserved
            .iter()
            .any(|&(a, b)| destination < b && a < dst_end)
        {
            return Err(format!(
                "RO ELF segment [{destination:#x}..{dst_end:#x}) in {} overlaps reserved slot/NVRAM area",
                path.display()
            ));
        }
        flash[destination..dst_end].copy_from_slice(source);
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
