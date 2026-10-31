//! Processor name string (CCX_SP3 §2 step 6b, instruction-checked
//! `ffdc24d8`): the shared [`crate::ccx::brand`] with each generation's SMU
//! message. All generations do this on a cold boot; the BSP's values are
//! later copied to the APs.

pub use crate::ccx::brand::{FALLBACK, Name, write};
use crate::smu::SmuMessages;
use crate::sp3::soc::Generation;

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
    crate::ccx::brand::read(smu, message(generation))
}
