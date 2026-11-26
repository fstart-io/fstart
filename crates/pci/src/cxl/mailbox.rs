//! A CXL memory device's registers (CXL 3.1 §8.2.8): the device capability
//! array of the Memory Device register block (Register Locator block 3)
//! and its primary mailbox, with the commands firmware sends to size a
//! Dynamic Capacity Device: Get Log of the Command Effects Log and Get
//! Dynamic Capacity Configuration (the vendors' `CxlIsDcdSupported`,
//! `CxlGetDcRegions`).

use tock_registers::{LocalRegisterCopy, register_bitfields};

use super::Registers;
use fstart_core::services::Timer;

/// Device capability IDs (CXL 3.1 Table 8-57).
pub mod capability {
    pub const DEVICE_STATUS: u16 = 1;
    pub const PRIMARY_MAILBOX: u16 = 2;
    pub const SECONDARY_MAILBOX: u16 = 3;
    pub const MEMORY_DEVICE_STATUS: u16 = 0x4000;
}

/// Command opcodes.
pub mod opcode {
    pub const GET_SUPPORTED_LOGS: u16 = 0x0400;
    pub const GET_LOG: u16 = 0x0401;
    pub const IDENTIFY_MEMORY_DEVICE: u16 = 0x4000;
    pub const GET_DC_CONFIGURATION: u16 = 0x4800;
}

/// The Command Effects Log's UUID `0da9c0b5-bf41-4b78-8f79-96b1623b3f17`
/// in its on-wire byte order.
pub const CEL_UUID: [u8; 16] = [
    0xb5, 0xc0, 0xa9, 0x0d, 0x41, 0xbf, 0x78, 0x4b, 0x8f, 0x79, 0x96, 0xb1, 0x62, 0x3b, 0x3f, 0x17,
];

/// Mailbox register offsets from the mailbox capability.
pub const MB_CAPABILITIES: u32 = 0x00;
pub const MB_CONTROL: u32 = 0x04;
pub const MB_COMMAND: u32 = 0x08;
pub const MB_STATUS: u32 = 0x10;
pub const MB_PAYLOAD: u32 = 0x20;

/// How long a command may take (CXL 3.1 §8.2.8.4: 2 s).
pub const TIMEOUT_US: u64 = 2_000_000;
/// Bytes of one Dynamic Capacity region configuration (CXL 3.1 Table
/// 8-165; the vendors copy with a 0x29 stride, which is not the spec's).
pub const DC_REGION_SIZE: usize = 0x28;
/// Most DC regions (8).
pub const MAX_DC_REGIONS: usize = 8;

register_bitfields![u32,
    /// Mailbox Capabilities +0x00 (32-bit).
    pub MbCapabilities [
        /// log2 of the payload area's bytes (8..20).
        PAYLOAD_SIZE OFFSET(0) NUMBITS(5) []
    ],
    /// Mailbox Control +0x04 (32-bit).
    pub MbControl [
        DOORBELL OFFSET(0) NUMBITS(1) []
    ],
    /// Mailbox Command +0x08 (low dword).
    pub MbCommand [
        OPCODE OFFSET(0) NUMBITS(16) [],
        /// Payload length bits 15:0 (bits 20:16 in the high dword).
        LENGTH_LOW OFFSET(16) NUMBITS(16) []
    ],
    /// Mailbox Status +0x10 (high dword, +0x14).
    pub MbStatusHigh [
        RETURN_CODE OFFSET(0) NUMBITS(16) []
    ]
];

/// Why a command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The block has no primary mailbox.
    NoMailbox,
    /// The doorbell never cleared.
    Timeout,
    /// The device answered this return code.
    Return(u16),
    /// The input or output does not fit the payload area.
    Payload,
}

/// The offset of device capability `id` in the Memory Device register
/// block `regs` (the capability array, CXL 3.1 §8.2.8.1).
pub fn find_capability(regs: &impl Registers, id: u16) -> Option<u32> {
    let count = regs.read32(4) & 0xffff;
    (1..=count.min(64)).find_map(|n| {
        let header = 0x10 * n;
        (regs.read32(header) & 0xffff == u32::from(id)).then(|| regs.read32(header + 4))
    })
}

/// Memory Device Status (capability [`capability::MEMORY_DEVICE_STATUS`],
/// +0, 64-bit; its low dword).
pub mod memory_device_status {
    pub const DEVICE_FATAL: u32 = 1 << 0;
    pub const FW_HALT: u32 = 1 << 1;
    /// Media Status (bits 3:2): 1 ready.
    pub const MEDIA_STATUS_SHIFT: u32 = 2;
    pub const MEDIA_READY: u32 = 1;
    pub const MAILBOX_READY: u32 = 1 << 4;

    /// The media is ready and the device neither fatal nor halted.
    pub const fn ready(status: u32) -> bool {
        status & (DEVICE_FATAL | FW_HALT) == 0 && (status >> MEDIA_STATUS_SHIFT) & 3 == MEDIA_READY
    }
}

/// The primary mailbox of a memory device register block.
pub struct Mailbox<R> {
    regs: R,
    base: u32,
    payload: usize,
}

impl<R: Registers> Mailbox<R> {
    /// The primary mailbox of the Memory Device register block `regs`.
    pub fn find(regs: R) -> Result<Self, Error> {
        let base = find_capability(&regs, capability::PRIMARY_MAILBOX).ok_or(Error::NoMailbox)?;
        let caps = LocalRegisterCopy::<u32, MbCapabilities::Register>::new(
            regs.read32(base + MB_CAPABILITIES),
        );
        let payload = 1usize << caps.read(MbCapabilities::PAYLOAD_SIZE).clamp(8, 20);
        Ok(Self {
            regs,
            base,
            payload,
        })
    }

    /// Bytes of the payload area.
    pub fn payload_size(&self) -> usize {
        self.payload
    }

    fn doorbell(&self) -> bool {
        LocalRegisterCopy::<u32, MbControl::Register>::new(self.regs.read32(self.base + MB_CONTROL))
            .is_set(MbControl::DOORBELL)
    }

    /// Run `opcode` with `input`; the output goes into `output`. Returns
    /// the output's length the device reported.
    pub fn command(
        &self,
        opcode: u16,
        input: &[u8],
        output: &mut [u8],
        timer: &dyn Timer,
    ) -> Result<usize, Error> {
        if input.len() > self.payload || !input.len().is_multiple_of(4) {
            return Err(Error::Payload);
        }
        let ready = || (!self.doorbell()).then_some(());
        fstart_core::services::timer::poll_until(timer, TIMEOUT_US, ready).ok_or(Error::Timeout)?;
        for (i, chunk) in input.as_chunks::<4>().0.iter().enumerate() {
            let dword = u32::from_le_bytes(*chunk);
            self.regs
                .write32(self.base + MB_PAYLOAD + 4 * i as u32, dword);
        }
        let mut command = LocalRegisterCopy::<u32, MbCommand::Register>::new(0);
        command.write(
            MbCommand::OPCODE.val(opcode.into())
                + MbCommand::LENGTH_LOW.val(input.len() as u32 & 0xffff),
        );
        self.regs.write32(self.base + MB_COMMAND, command.get());
        self.regs
            .write32(self.base + MB_COMMAND + 4, (input.len() >> 16) as u32);
        let mut control = LocalRegisterCopy::<u32, MbControl::Register>::new(0);
        control.write(MbControl::DOORBELL::SET);
        self.regs.write32(self.base + MB_CONTROL, control.get());
        fstart_core::services::timer::poll_until(timer, TIMEOUT_US, ready).ok_or(Error::Timeout)?;
        let status = LocalRegisterCopy::<u32, MbStatusHigh::Register>::new(
            self.regs.read32(self.base + MB_STATUS + 4),
        );
        let code = status.read(MbStatusHigh::RETURN_CODE) as u16;
        if code != 0 {
            return Err(Error::Return(code));
        }
        let length = (self.regs.read32(self.base + MB_COMMAND) >> 16) as usize
            | ((self.regs.read32(self.base + MB_COMMAND + 4) as usize & 0x1f) << 16);
        let length = length.min(self.payload);
        let n = length.min(output.len());
        for (i, chunk) in output[..n].chunks_mut(4).enumerate() {
            let dword = self.regs.read32(self.base + MB_PAYLOAD + 4 * i as u32);
            chunk.copy_from_slice(&dword.to_le_bytes()[..chunk.len()]);
        }
        Ok(length)
    }

    /// Whether the Command Effects Log lists `opcode` (Get Log of the
    /// CEL, entries of 4 bytes: opcode, effect).
    pub fn cel_lists(&self, opcode: u16, timer: &dyn Timer) -> Result<bool, Error> {
        let mut offset = 0u32;
        let mut chunk = [0u8; 256];
        loop {
            let mut input = [0u8; 24];
            input[..16].copy_from_slice(&CEL_UUID);
            input[16..20].copy_from_slice(&offset.to_le_bytes());
            input[20..24].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
            let n = self.command(opcode::GET_LOG, &input, &mut chunk, timer)?;
            let n = n.min(chunk.len());
            if chunk[..n]
                .as_chunks::<4>()
                .0
                .iter()
                .any(|e| u16::from_le_bytes([e[0], e[1]]) == opcode)
            {
                return Ok(true);
            }
            if n < chunk.len() {
                return Ok(false);
            }
            offset += n as u32;
            if offset > 0x10_0000 {
                return Ok(false);
            }
        }
    }

    /// Get Dynamic Capacity Configuration (`0x4800`, input region count 8,
    /// start region 0): the regions' lengths (record `+0x10`).
    pub fn dc_region_lengths(
        &self,
        timer: &dyn Timer,
    ) -> Result<heapless::Vec<u64, MAX_DC_REGIONS>, Error> {
        let input = [MAX_DC_REGIONS as u8, 0, 0, 0];
        let mut output = [0u8; 8 + MAX_DC_REGIONS * DC_REGION_SIZE];
        let n = self.command(opcode::GET_DC_CONFIGURATION, &input, &mut output, timer)?;
        let available = usize::from(output[0]).min(MAX_DC_REGIONS);
        let mut lengths = heapless::Vec::new();
        for r in 0..available {
            let at = 8 + r * DC_REGION_SIZE + 0x10;
            if at + 8 > n.min(output.len()) {
                break;
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&output[at..at + 8]);
            let _ = lengths.push(u64::from_le_bytes(bytes));
        }
        Ok(lengths)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use fstart_core::services::timer::ManualTimer;
    use std::vec::Vec;

    /// A memory device block: the array (1 capability), the primary
    /// mailbox at 0x100 with a 1 KiB payload; commands complete at once.
    struct Device {
        regs: RefCell<Vec<u32>>,
        cel: Vec<u16>,
    }

    impl Registers for &Device {
        fn read32(&self, offset: u32) -> u32 {
            self.regs.borrow()[offset as usize / 4]
        }
        fn write32(&self, offset: u32, value: u32) {
            let mut regs = self.regs.borrow_mut();
            regs[offset as usize / 4] = value;
            if offset == 0x104 && value & 1 != 0 {
                let opcode = regs[0x108 / 4] as u16;
                let payload = 0x120 / 4;
                let out: Vec<u8> = match opcode {
                    opcode::GET_LOG => self
                        .cel
                        .iter()
                        .flat_map(|o| [o.to_le_bytes(), [0, 0]].concat())
                        .collect(),
                    opcode::GET_DC_CONFIGURATION => {
                        let mut o = std::vec![0u8; 8 + 2 * DC_REGION_SIZE];
                        o[0] = 2;
                        o[8 + 0x10..8 + 0x18].copy_from_slice(&(1u64 << 30).to_le_bytes());
                        o[8 + DC_REGION_SIZE + 0x10..8 + DC_REGION_SIZE + 0x18]
                            .copy_from_slice(&(3u64 << 30).to_le_bytes());
                        o
                    }
                    _ => Vec::new(),
                };
                for (i, c) in out.chunks(4).enumerate() {
                    let mut d = [0u8; 4];
                    d[..c.len()].copy_from_slice(c);
                    regs[payload + i] = u32::from_le_bytes(d);
                }
                regs[0x108 / 4] = u32::from(opcode) | (out.len() as u32) << 16;
                regs[0x104 / 4] = 0;
            }
        }
    }

    #[test]
    fn dcd_sizing_through_the_primary_mailbox() {
        let mut regs = std::vec![0u32; 0x800];
        regs[1] = 1; // one capability
        regs[4] = u32::from(capability::PRIMARY_MAILBOX);
        regs[5] = 0x100;
        regs[0x100 / 4] = 10; // 1 KiB payload
        let device = Device {
            regs: RefCell::new(regs),
            cel: std::vec![opcode::IDENTIFY_MEMORY_DEVICE, opcode::GET_DC_CONFIGURATION],
        };
        let timer = ManualTimer::new();
        assert_eq!(
            find_capability(&&device, capability::PRIMARY_MAILBOX),
            Some(0x100)
        );
        assert!(memory_device_status::ready(0b10100));
        assert!(!memory_device_status::ready(0b01000));
        let mailbox = Mailbox::find(&device).unwrap();
        assert_eq!(mailbox.payload_size(), 1024);
        assert_eq!(
            mailbox.cel_lists(opcode::GET_DC_CONFIGURATION, &timer),
            Ok(true)
        );
        assert_eq!(mailbox.cel_lists(0x4801, &timer), Ok(false));
        assert_eq!(
            mailbox.dc_region_lengths(&timer).unwrap()[..],
            [1 << 30, 3 << 30]
        );
    }
}
