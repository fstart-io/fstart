//! Processor name string (CCX_SP3 §2 step 6b, instruction-checked
//! `ffdc24d8`): 47 bytes from the SMU, written to C001_0030–C001_0035
//! (CPUID Fn8000_0002–4 read them back). All generations do this on a cold
//! boot; the BSP's values are later copied to the APs.

use fstart_arch::x86::cpu::msr::MsrAccess;

use crate::smu::{SMU_ARGS, SmuMessages};
use crate::soc::Generation;

/// First of the six name-string MSRs.
pub const NAME_STRING: u32 = 0xc001_0030;
/// The donors' string when the SMU has none.
pub const FALLBACK: &[u8] = b"AMD Unprogrammed Engineering Sample";

/// The 48-byte, NUL-terminated name.
pub type Name = [u8; 48];

/// SMU message reading name dword `i`: V9 GetNameString 0x28, V11
/// GetBrandString 0x0D (NBIO_SMU §3.8).
const fn message(generation: Generation) -> u32 {
    match generation {
        Generation::Naples => 0x28,
        _ => 0x0d,
    }
}

/// Read the name from the SMU of socket 0 (die 0 on Naples).
pub fn read<M: SmuMessages + ?Sized>(generation: Generation, smu: &M) -> Name {
    let mut name = [0u8; 48];
    let ok = name
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
        .all(|(i, chunk)| {
            let mut args = [0; SMU_ARGS];
            args[0] = i as u32;
            let sent = smu.send(message(generation), &mut args).is_ok();
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

/// Write the name MSRs of the executing thread.
pub fn write<M: MsrAccess + ?Sized>(cpu: &M, name: &Name) {
    name.as_chunks::<8>()
        .0
        .iter()
        .zip(NAME_STRING..)
        .for_each(|(c, msr)| cpu.write(msr, u64::from_le_bytes(*c)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smu::SmuError;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    struct Smu(&'static [u8]);
    impl SmuMessages for Smu {
        fn send(&self, id: u32, args: &mut [u32; SMU_ARGS]) -> Result<(), SmuError> {
            assert_eq!(id, 0x0d);
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
        let name = read(Generation::Rome, &Smu(b"AMD EPYC 7702P 64-Core Processor"));
        assert_eq!(&name[..32], b"AMD EPYC 7702P 64-Core Processor");
        let cpu = MsrSpace::new([]);
        write(&cpu, &name);
        let cpu = cpu.into_inner();
        assert_eq!(cpu.get(0xc001_0030), u64::from_le_bytes(*b"AMD EPYC"));
        assert_eq!(cpu.writes.len(), 6);
        let empty = read(Generation::Milan, &Smu(b""));
        assert_eq!(&empty[..FALLBACK.len()], FALLBACK);
    }
}
