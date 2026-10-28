//! Add WC apertures without rebuilding vendor-specific RAM/cache policy.
use super::*;
use crate::x86::{cpu::msr::PAT, mp::MpHandle};

struct Update {
    // Only previously disabled variable MTRRs are changed.
    writes: Vec<(u32, u64, u64), MAX_MTRRS>,
}

impl Update {
    fn build(
        caps: Capabilities,
        pairs: &[(u64, u64)],
        ranges: &[(u64, u64)],
    ) -> Result<Self, ServiceError> {
        if !(32..=52).contains(&caps.physical_bits)
            || !caps.write_combining
            || caps.variable_count > MAX_MTRRS
            || pairs.len() != caps.variable_count
        {
            return Err(ServiceError::NotSupported);
        }
        let physical = (caps.limit() - 1) & !0xfff;
        let occupied = pairs
            .iter()
            .copied()
            .filter(|(_, mask)| is_valid_mask(*mask))
            .map(|(base, mask)| {
                let size = (!(mask & physical) & physical) + 0x1000;
                let start = base & physical;
                let end = start.checked_add(size).ok_or(ServiceError::InvalidParam)?;
                if !size.is_power_of_two() || start & (size - 1) != 0 || end > caps.limit() {
                    return Err(ServiceError::InvalidParam);
                }
                Ok((start..end, decode_type(base)))
            })
            .collect::<Result<Vec<_, MAX_MTRRS>, ServiceError>>()?;
        let mut free = pairs
            .iter()
            .enumerate()
            .filter(|(_, (_, mask))| !is_valid_mask(*mask))
            .map(|(index, _)| index as u32);
        let mut writes = Vec::new();
        for &(base, size) in ranges.iter().filter(|(_, size)| *size != 0) {
            let end = base.checked_add(size).ok_or(ServiceError::InvalidParam)?;
            if base < ONE_MIB || base & 0xfff != 0 || end & 0xfff != 0 || end > caps.limit() {
                return Err(ServiceError::InvalidParam);
            }
            if occupied.iter().any(|(range, kind)| {
                base < range.end && range.start < end && *kind != MTRR_TYPE_WRITE_COMBINING
            }) {
                // UC wins over WC; WB/WC overlap is undefined. Never silently
                // install an ineffective or conflicting cache type.
                return Err(ServiceError::NotSupported);
            }
            if occupied
                .iter()
                .any(|(range, _)| range.start <= base && end <= range.end)
            {
                continue;
            }
            for (base, size) in chunks(base, size) {
                let index = free.next().ok_or(ServiceError::NotSupported)?;
                writes
                    .push((
                        index,
                        base | MTRR_TYPE_WRITE_COMBINING,
                        (!(size - 1) & physical) | VALID,
                    ))
                    .map_err(|_| ServiceError::NotSupported)?;
            }
        }
        Ok(Self { writes })
    }
}

fn arrive(counter: &AtomicUsize, participants: usize) {
    counter.fetch_add(1, Ordering::AcqRel);
    while counter.load(Ordering::Acquire) != participants {
        core::hint::spin_loop();
    }
}

/// Add exact, page-aligned prefetchable framebuffer apertures on every CPU.
/// Existing RAM, ROM, fixed MTRRs, PAT and vendor routing (including AMD
/// TOM2WB/RdDram/WrDram) are preserved. Failure to plan changes nothing.
///
/// # Safety
/// RAM-stage BSP only, never CAR. The caller owns MP dispatch, excludes
/// interrupts/asynchronous cache-policy writers, and guarantees these ranges
/// are allocated prefetchable memory, not side-effecting device registers.
/// Every online CPU must support MTRRs, have caching enabled, and still
/// participate in the supplied MP handle.
pub unsafe fn install_write_combining(
    ranges: impl IntoIterator<Item = (u64, u64)>,
    mp: &MpHandle,
) -> Result<(), ServiceError> {
    let mut requested: Vec<_, MAX_RANGES> = Vec::new();
    for range in ranges.into_iter().filter(|(_, size)| *size != 0) {
        requested
            .push(range)
            .map_err(|_| ServiceError::NotSupported)?;
    }
    if requested.is_empty() {
        return Ok(());
    }
    // SAFETY: ring 0, MTRRs supported, BSP owns cache-policy programming.
    let caps = unsafe { Capabilities::current() };
    if caps.variable_count > MAX_MTRRS {
        return Err(ServiceError::NotSupported);
    }
    let pairs: Vec<_, MAX_MTRRS> = (0..caps.variable_count as u32)
        .map(|index| unsafe { read_variable(index) })
        .collect();
    let default = unsafe { rdmsr(IA32_MTRR_DEF_TYPE) };
    if default & ENABLE == 0 {
        return Err(ServiceError::NotInitialized);
    }
    let update = Update::build(caps, &pairs, &requested)?;
    // Identity mappings use PAT entry 0 (WB). MTRR WC combined with PAT WB
    // is WC. Do not claim WC if the page-level policy would override it.
    let pat = (crate::x86::cpuid(1).3 & (1 << 16) != 0).then(|| unsafe { rdmsr(PAT.index()) });
    if pat.is_some_and(|pat| pat & 0xff != MTRR_TYPE_WRITE_BACK) {
        return Err(ServiceError::NotSupported);
    }
    let failed = AtomicBool::new(false);
    let phases = [const { AtomicUsize::new(0) }; 5];
    let participants = usize::from(mp.num_cpus());
    mp.scope(|scope| {
        scope.rendezvous(&|_| {
            // Preflight every CPU before any CPU changes its cache state.
            let matches = unsafe {
                Capabilities::current() == caps
                    && rdmsr(IA32_MTRR_DEF_TYPE) == default
                    && !x86::controlregs::cr0().contains(x86::controlregs::Cr0::CR0_CACHE_DISABLE)
                    && (crate::x86::cpuid(1).3 & (1 << 16) != 0) == pat.is_some()
                    && pat.is_none_or(|pat| rdmsr(PAT.index()) == pat)
                    && pairs
                        .iter()
                        .enumerate()
                        .all(|(index, pair)| read_variable(index as u32) == *pair)
            };
            if !matches {
                failed.store(true, Ordering::Release);
            }
            arrive(&phases[0], participants);
            if failed.load(Ordering::Acquire) || update.writes.is_empty() {
                return;
            }
            // SAFETY: every CPU agreed on the same immutable plan. Barriers also
            // cover SMT siblings sharing MTRRs; no CPU restores a register while
            // another sibling can still disable it or change a mask.
            unsafe {
                let cr4 = x86::controlregs::cr4();
                let cr3 = x86::controlregs::cr3();
                // Supported i945/Atom parts must never WBINVD with CR0.CD=1.
                crate::x86::writeback_invalidate_caches();
                disable_cache();
                x86::controlregs::cr4_write(cr4 & !x86::controlregs::Cr4::CR4_ENABLE_GLOBAL_PAGES);
                x86::controlregs::cr3_write(cr3);
                arrive(&phases[1], participants);
                wrmsr(IA32_MTRR_DEF_TYPE, default & !ENABLE);
                arrive(&phases[2], participants);
                for &(index, base, mask) in &update.writes {
                    wrmsr(IA32_MTRR_PHYSMASK0 + 2 * index, 0);
                    wrmsr(IA32_MTRR_PHYSBASE0 + 2 * index, base);
                    wrmsr(IA32_MTRR_PHYSMASK0 + 2 * index, mask);
                }
                arrive(&phases[3], participants);
                wrmsr(IA32_MTRR_DEF_TYPE, default);
                arrive(&phases[4], participants);
                if update
                    .writes
                    .iter()
                    .any(|&(index, base, mask)| read_variable(index) != (base, mask))
                {
                    failed.store(true, Ordering::Release);
                }
                x86::controlregs::cr3_write(cr3);
                x86::controlregs::cr4_write(cr4);
                enable_cache();
            }
        })
    });
    if failed.load(Ordering::Acquire) {
        return Err(ServiceError::HardwareError);
    }
    for (base, size) in requested {
        fstart_log::info!(
            "mtrr: WC {:#x}..{:#x}, verified on {} CPUs",
            base,
            base + size,
            participants
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn caps(count: usize) -> Capabilities {
        Capabilities {
            variable_count: count,
            physical_bits: 40,
            fixed: true,
            write_combining: true,
        }
    }
    fn pair(base: u64, size: u64, kind: u64) -> (u64, u64) {
        (base | kind, (!(size - 1) & ((1 << 40) - 0x1000)) | VALID)
    }
    #[test]
    fn preserves_ram_and_rom_using_only_disabled_slots() {
        let pairs = [
            pair(0, 0x80000000, 6),
            (0, 0),
            pair(0xff000000, 0x1000000, 5),
            (0, 0),
        ];
        let update = Update::build(caps(4), &pairs, &[(0xe0000000, 0x10000000)]).unwrap();
        let (base, mask) = pair(0xe0000000, 0x10000000, 1);
        assert_eq!(update.writes.as_slice(), &[(1, base, mask)]);
    }
    #[test]
    fn exact_decomposition_does_not_round_wc_into_adjacent_registers() {
        let update = Update::build(caps(2), &[(0, 0); 2], &[(0xe0200000, 0x600000)]).unwrap();
        let (a, b) = pair(0xe0200000, 0x200000, 1);
        let (c, d) = pair(0xe0400000, 0x400000, 1);
        assert_eq!(update.writes.as_slice(), &[(0, a, b), (1, c, d)]);
        assert!(Update::build(caps(1), &[(0, 0)], &[(0xe0200000, 0x600000)]).is_err());
    }
    #[test]
    fn rejects_conflicts_unsupported_wc_and_invalid_spans() {
        for kind in [0, 5, 6] {
            assert!(
                Update::build(
                    caps(2),
                    &[pair(0xe0000000, 0x10000000, kind), (0, 0)],
                    &[(0xe0000000, 0x1000000)]
                )
                .is_err()
            );
        }
        let no_wc = Capabilities {
            write_combining: false,
            ..caps(1)
        };
        assert!(Update::build(no_wc, &[(0, 0)], &[(0xe0000000, 0x1000000)]).is_err());
        for range in [
            (0xe0000001, 0x1000),
            (0xe0000000, 0x1001),
            (0, 0x1000),
            (u64::MAX - 0xfff, 0x2000),
        ] {
            assert!(Update::build(caps(1), &[(0, 0)], &[range]).is_err());
        }
    }
    #[test]
    fn already_wc_needs_no_free_mtrrs() {
        assert!(
            Update::build(
                caps(1),
                &[pair(0xe0000000, 0x10000000, 1)],
                &[(0xe0000000, 0x1000000)]
            )
            .unwrap()
            .writes
            .is_empty()
        );
    }
}
