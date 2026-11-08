//! The head of an AST2400/AST2500 SPI boot image.
//!
//! The ARM core executes FMC CE0 flash in place from address 0, so the image
//! opens with the ARM vector table. Before releasing the CPU, the SoC scans
//! the first 8 KiB of the flash for a strap block and replays its register
//! writes. The head places the strap block right after the vectors and
//! their handler addresses, at offset 0x40.

/// One register write the SoC replays from flash before the CPU starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrapWrite {
    /// Register address; must lie in `0x1Exx_xxxx` and be word aligned.
    pub addr: u32,
    pub value: u32,
}

impl StrapWrite {
    pub const fn new(addr: u32, value: u32) -> Self {
        Self { addr, value }
    }

    /// The 8-byte command: address bytes [31:24], [23:16], [15:8], then
    /// address bits [7:2] with the even parities of address bits [23:2]
    /// (bit 25) and of the data (bit 24); then the data word.
    const fn encode(self) -> [u32; 2] {
        assert!(
            self.addr >> 24 == 0x1E,
            "strap writes reach 0x1Exx_xxxx only"
        );
        assert!(self.addr & 3 == 0, "strap writes are word aligned");
        let addr_parity = ((self.addr >> 2) & 0x3F_FFFF).count_ones() & 1;
        let data_parity = self.value.count_ones() & 1;
        let word = (self.addr >> 24)
            | ((self.addr >> 16) & 0xFF) << 8
            | ((self.addr >> 8) & 0xFF) << 16
            | addr_parity << 25
            | data_parity << 24
            | ((self.addr >> 2) & 0x3F) << 26;
        [word, self.value]
    }
}

/// `*ASTHwStrap#`
const START_CODE: [u32; 3] = [0x5453_412A, 0x7453_7748, 0x2370_6172];
/// `TSA*`
const END_CODE: u32 = 0x2A41_5354;

/// Start code, the commands, end code.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrapBlock<const N: usize> {
    start: [u32; 3],
    commands: [[u32; 2]; N],
    end: u32,
}

impl<const N: usize> StrapBlock<N> {
    pub const fn new(writes: [StrapWrite; N]) -> Self {
        let mut commands = [[0; 2]; N];
        let mut i = 0;
        while i < N {
            commands[i] = writes[i].encode();
            i += 1;
        }
        Self {
            start: START_CODE,
            commands,
            end: END_CODE,
        }
    }
}

/// `ldr pc, [pc, #0x18]`: each vector loads its handler from the word 0x20
/// bytes further on.
#[cfg(target_arch = "arm")]
const LDR_PC_HANDLER: u32 = 0xE59F_F018;

#[cfg(target_arch = "arm")]
type Handler = unsafe extern "C" fn() -> !;

/// Vectors at 0x00, their handlers at 0x20, the strap block at 0x40.
#[cfg(target_arch = "arm")]
#[repr(C)]
pub struct SpiHead<const N: usize> {
    vectors: [u32; 8],
    handlers: [Handler; 8],
    strap: StrapBlock<N>,
}

#[cfg(target_arch = "arm")]
unsafe extern "C" {
    /// The arch entry.
    fn _start() -> !;
    /// The arch's stackless loop for unhandled exceptions: they enter modes
    /// whose banked stack pointers were never set up.
    fn _exception_hang() -> !;
}

#[cfg(target_arch = "arm")]
impl<const N: usize> SpiHead<N> {
    /// Reset enters the arch `_start`; every exception halts.
    pub const fn new(writes: [StrapWrite; N]) -> Self {
        let mut handlers: [Handler; 8] = [_exception_hang; 8];
        handlers[0] = _start;
        Self {
            vectors: [LDR_PC_HANDLER; 8],
            handlers,
            strap: StrapBlock::new(writes),
        }
    }
}

/// Place a board's SPI image head: `spi_head!(STRAP_WRITES);` with
/// `STRAP_WRITES: [StrapWrite; N]`.
#[macro_export]
macro_rules! spi_head {
    ($writes:path) => {
        #[used]
        #[unsafe(link_section = ".head.aspeed")]
        static ASPEED_SPI_HEAD: $crate::head::SpiHead<{ $writes.len() }> =
            $crate::head::SpiHead::new($writes);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes<const N: usize>(block: &StrapBlock<N>) -> &[u8] {
        // SAFETY: StrapBlock is repr(C) and made of u32s only.
        unsafe {
            core::slice::from_raw_parts(
                (block as *const StrapBlock<N>).cast::<u8>(),
                core::mem::size_of::<StrapBlock<N>>(),
            )
        }
    }

    #[test]
    fn encodes_a_32_mib_vga_strap_block() {
        // Unlock the SCU, set SCU70 bit 3 and clear SCU70 bit 2 through
        // SCU7C, which straps a 32 MiB VGA memory carve-out.
        const EXPECTED: [u8; 40] = [
            0x2a, 0x41, 0x53, 0x54, 0x48, 0x77, 0x53, 0x74, 0x72, 0x61, 0x70, 0x23, 0x1e, 0x6e,
            0x20, 0x01, 0xa8, 0xa8, 0x88, 0x16, 0x1e, 0x6e, 0x20, 0x73, 0x08, 0x00, 0x00, 0x00,
            0x1e, 0x6e, 0x20, 0x7f, 0x04, 0x00, 0x00, 0x00, 0x54, 0x53, 0x41, 0x2a,
        ];
        let block = StrapBlock::new([
            StrapWrite::new(0x1E6E_2000, 0x1688_A8A8),
            StrapWrite::new(0x1E6E_2070, 0x0000_0008),
            StrapWrite::new(0x1E6E_207C, 0x0000_0004),
        ]);
        assert_eq!(bytes(&block), EXPECTED);
    }

    #[test]
    fn empty_strap_block_is_start_and_end_code() {
        let block = StrapBlock::<0>::new([]);
        assert_eq!(bytes(&block), b"*ASTHwStrap#TSA*");
    }
}
