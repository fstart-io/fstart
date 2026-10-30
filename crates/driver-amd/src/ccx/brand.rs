// SPDX-License-Identifier: MIT OR Apache-2.0
//! The processor name string: 47 bytes the BIOS reads from the SMU and
//! writes to C001_0030–C001_0035 on every thread, where CPUID Fn8000_0002–4
//! read them back (the vendor CCX code of Zeppelin, Naples, Rome and Milan).
use super::msr::NAME_STRING;
use crate::smu::{ARGUMENT_COUNT, SmuMessages};
use fstart_arch::x86::cpu::msr::MsrAccess;
use tock_registers::interfaces::Writeable;

/// The vendor string when the SMU has none.
pub const FALLBACK: &[u8] = b"AMD Unprogrammed Engineering Sample";

/// The 48-byte, NUL-terminated name.
pub type Name = [u8; 48];

/// Read the name with the SMU's `get_name` message (dword index in, dword
/// out), the fallback if the SMU doesn't answer or has none.
pub fn read<M: SmuMessages + ?Sized>(smu: &M, get_name: u32) -> Name {
    let mut name = [0u8; 48];
    let ok = name
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
        .all(|(i, chunk)| {
            let mut args = [0; ARGUMENT_COUNT];
            args[0] = i as u32;
            let sent = smu.send(get_name, &mut args).is_ok();
            *chunk = args[0].to_le_bytes();
            sent
        });
    if !ok || name[0] == 0 {
        name = [0; 48];
        name[..FALLBACK.len()].copy_from_slice(FALLBACK);
    }
    name[47] = 0;
    name
}

/// The name up to its NUL, for logs.
pub fn as_str(name: &Name) -> &str {
    let length = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    core::str::from_utf8(&name[..length]).unwrap_or("?")
}

/// Write the name MSRs of the executing thread.
pub fn write<M: MsrAccess + ?Sized>(cpu: &M, name: &Name) {
    name.as_chunks::<8>()
        .0
        .iter()
        .zip(0..)
        .for_each(|(c, i)| cpu.reg(NAME_STRING.offset(i)).set(u64::from_le_bytes(*c)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smu::Error as SmuError;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    struct Smu(&'static [u8]);
    impl SmuMessages for Smu {
        fn send(&self, id: u32, args: &mut [u32; ARGUMENT_COUNT]) -> Result<(), SmuError> {
            assert_eq!(id, 0x28);
            let i = args[0] as usize * 4;
            let mut b = [0u8; 4];
            b.iter_mut()
                .zip(self.0.iter().skip(i))
                .for_each(|(d, s)| *d = *s);
            args[0] = u32::from_le_bytes(b);
            Ok(())
        }
    }

    #[test]
    fn name_to_msrs() {
        let name = read(&Smu(b"AMD EPYC 3151 4-Core Processor"), 0x28);
        assert_eq!(as_str(&name), "AMD EPYC 3151 4-Core Processor");
        let cpu = MsrSpace::new([]);
        write(&cpu, &name);
        let cpu = cpu.into_inner();
        assert_eq!(cpu.get(0xc001_0030), u64::from_le_bytes(*b"AMD EPYC"));
        assert_eq!(cpu.writes.len(), 6);
        let empty = read(&Smu(b""), 0x28);
        assert_eq!(&empty[..FALLBACK.len()], FALLBACK);
    }
}
