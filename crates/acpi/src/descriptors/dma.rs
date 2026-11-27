//! Fixed DMA resource descriptor (ACPI 6.5 section 6.4.2.8).
//!
//! Unlike the legacy ISA DMA descriptor, FixedDMA encodes request line,
//! channel, and transfer width separately (used by Intel Serial IO).

use acpi_tables::{Aml, AmlSink};
use zerocopy::little_endian::U16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct FixedDmaDescriptor {
    tag: u8,
    request: U16,
    channel: U16,
    width: u8,
}

const _: () = assert!(core::mem::size_of::<FixedDmaDescriptor>() == 6);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TransferWidth {
    Bits8 = 0,
    Bits16 = 1,
    Bits32 = 2,
    Bits64 = 3,
    Bits128 = 4,
    Bits256 = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedDma {
    pub request: u16,
    pub channel: u16,
    pub width: TransferWidth,
}

impl Aml for FixedDma {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        let desc = FixedDmaDescriptor {
            tag: 0x55,
            request: U16::new(self.request),
            channel: U16::new(self.channel),
            width: self.width as u8,
        };
        sink.vec(desc.as_bytes());
    }
}
