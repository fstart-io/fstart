//! Sparse cold-boot DRAM integrity probes. These are not a full memory test.

use fstart_core::services::ServiceError;

const MIB: u32 = 1 << 20;
const WORD: u32 = core::mem::size_of::<u32>() as u32;
const PROBES: usize = 6;

type Windows = [(u32, u32); 2];

pub(super) fn validate_windows(windows: Windows) -> Result<(), ServiceError> {
    if windows.iter().any(|&(base, end)| {
        base < MIB || base & (WORD - 1) != 0 || end & (WORD - 1) != 0 || end <= base
    }) || !(windows[0].1 <= windows[1].0 || windows[1].1 <= windows[0].0)
    {
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

fn probe_addresses(usable_top: u32, windows: Windows) -> Result<[u32; PROBES], ServiceError> {
    validate_windows(windows)?;
    if usable_top <= MIB
        || usable_top & (MIB - 1) != 0
        || windows.iter().any(|&(_, end)| end > usable_top)
    {
        return Err(ServiceError::HardwareError);
    }
    // Select generic locations outside both linked destinations. Jump over
    // entire windows instead of scanning potentially large DRAM intervals.
    let mut low = MIB;
    for _ in 0..2 {
        if let Some(&(_, end)) = windows
            .iter()
            .find(|&&(base, end)| base <= low && low < end)
        {
            low = end;
        }
    }
    let mut high = usable_top - WORD;
    for _ in 0..2 {
        if let Some(&(base, _)) = windows
            .iter()
            .find(|&&(base, end)| base <= high && high < end)
        {
            high = base.checked_sub(WORD).ok_or(ServiceError::HardwareError)?;
        }
    }
    let addresses = [
        low,
        windows[0].0,
        windows[0].1 - WORD,
        windows[1].0,
        windows[1].1 - WORD,
        high,
    ];
    if addresses.iter().enumerate().any(|(i, &addr)| {
        addr < MIB
            || addr & (WORD - 1) != 0
            || addr.checked_add(WORD).is_none_or(|end| end > usable_top)
            || addresses[..i].contains(&addr)
    }) {
        return Err(ServiceError::HardwareError);
    }
    Ok(addresses)
}

trait ProbeMemory {
    fn read(&mut self, address: u32) -> u32;
    fn write(&mut self, address: u32, value: u32);
}

struct PhysicalMemory;
impl ProbeMemory for PhysicalMemory {
    fn read(&mut self, address: u32) -> u32 {
        // SAFETY: caller checked word alignment and low usable DRAM bounds;
        // the bootblock runs from ROM and has its stack in CAR.
        unsafe { core::ptr::read_volatile(address as usize as *const u32) }
    }
    fn write(&mut self, address: u32, value: u32) {
        // SAFETY: same bounds and execution constraints as read.
        unsafe { core::ptr::write_volatile(address as usize as *mut u32, value) }
    }
}

fn probe_all<M: ProbeMemory>(memory: &mut M, addresses: [u32; PROBES]) -> Result<(), ServiceError> {
    // Capture every original before modifying any location. Keep each phase's
    // writes resident across all addresses so aliased addresses disagree on
    // their unique address-derived pattern.
    let original = addresses.map(|addr| memory.read(addr));
    let mut failure = None;
    for phase in 0..5 {
        for &addr in &addresses {
            let pattern = pattern(addr, phase);
            memory.write(addr, pattern);
        }
        for &addr in &addresses {
            let expected = pattern(addr, phase);
            let actual = memory.read(addr);
            if actual != expected {
                failure = Some((addr, expected, actual));
                break;
            }
        }
        if failure.is_some() {
            break;
        }
    }
    for (&addr, &value) in addresses.iter().zip(original.iter()) {
        memory.write(addr, value);
    }
    if let Some((addr, expected, actual)) = failure {
        fstart_log::error!(
            "gm45 ramtest: {:#x} wrote {:#x}, read {:#x}",
            addr,
            expected,
            actual
        );
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

fn pattern(addr: u32, phase: usize) -> u32 {
    match phase {
        0 => 0,
        1 => u32::MAX,
        2 => 0x5555_5555,
        3 => 0xaaaa_aaaa,
        _ => addr.rotate_left(13) ^ 0xa5a5_5a5a,
    }
}

pub(super) fn sample(usable_top: u32, windows: Windows) -> Result<(), ServiceError> {
    let addresses = probe_addresses(usable_top, windows)?;
    probe_all(&mut PhysicalMemory, addresses)?;
    fstart_log::info!("gm45 ramtest: sparse low DRAM probes passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS: Windows = [(16 * MIB, 16 * MIB + 128 * 1024), (64 * MIB, 80 * MIB)];

    #[test]
    fn gm45_probe_addresses_follow_linked_windows() {
        assert_eq!(
            probe_addresses(128 * MIB, WINDOWS),
            Ok([
                MIB,
                16 * MIB,
                0x0101_fffc,
                64 * MIB,
                0x04ff_fffc,
                128 * MIB - WORD
            ])
        );
        let relocated = [(2 * MIB, 3 * MIB), (8 * MIB, 12 * MIB)];
        assert_eq!(
            probe_addresses(32 * MIB, relocated),
            Ok([
                MIB,
                2 * MIB,
                3 * MIB - WORD,
                8 * MIB,
                12 * MIB - WORD,
                32 * MIB - WORD
            ])
        );
        assert_eq!(
            probe_addresses(32 * MIB, [(MIB, 2 * MIB), (31 * MIB, 32 * MIB)]),
            Ok([
                2 * MIB,
                MIB,
                2 * MIB - WORD,
                31 * MIB,
                32 * MIB - WORD,
                31 * MIB - WORD
            ])
        );
    }

    #[test]
    fn gm45_probe_ranges_fail_closed() {
        assert!(probe_addresses(79 * MIB, WINDOWS).is_err());
        assert!(probe_addresses(80 * MIB - 1, WINDOWS).is_err());
        assert!(probe_addresses(u32::MAX, WINDOWS).is_err());
        assert!(probe_addresses(128 * MIB, [(MIB - WORD, 2 * MIB), WINDOWS[1]]).is_err());
        assert!(probe_addresses(128 * MIB, [(MIB, MIB), WINDOWS[1]]).is_err());
        assert!(probe_addresses(128 * MIB, [(MIB + 1, 2 * MIB), WINDOWS[1]]).is_err());
        assert!(probe_addresses(128 * MIB, [(MIB, 70 * MIB), WINDOWS[1]]).is_err());
        assert!(probe_addresses(128 * MIB, [(79 * MIB, 81 * MIB), WINDOWS[1]]).is_err());
    }

    struct FakeMemory {
        values: [u32; PROBES],
        // Address lines alias the last probe to the first.
        alias: bool,
        fail_read: bool,
        reads: usize,
    }
    impl ProbeMemory for FakeMemory {
        fn read(&mut self, addr: u32) -> u32 {
            self.reads += 1;
            if self.fail_read && self.reads > PROBES && addr == 1 {
                return 7;
            }
            self.values[if self.alias && addr == PROBES as u32 {
                0
            } else {
                addr as usize - 1
            }]
        }
        fn write(&mut self, addr: u32, value: u32) {
            self.values[if self.alias && addr == PROBES as u32 {
                0
            } else {
                addr as usize - 1
            }] = value;
        }
    }

    #[test]
    fn gm45_probes_detect_alias_and_restore_originals() {
        let addresses = [1, 2, 3, 4, 5, 6];
        let original = [10, 20, 30, 40, 50, 60];
        let mut normal = FakeMemory {
            values: original,
            alias: false,
            fail_read: false,
            reads: 0,
        };
        assert!(probe_all(&mut normal, addresses).is_ok());
        assert_eq!(normal.values, original);
        let mut alias = FakeMemory {
            values: [10, 20, 30, 40, 50, 10],
            alias: true,
            fail_read: false,
            reads: 0,
        };
        assert!(probe_all(&mut alias, addresses).is_err());
        assert_eq!(alias.values, [10, 20, 30, 40, 50, 10]);
        let mut bad = FakeMemory {
            values: original,
            alias: false,
            fail_read: true,
            reads: 0,
        };
        assert!(probe_all(&mut bad, addresses).is_err());
        assert_eq!(bad.values, original);
    }
}
