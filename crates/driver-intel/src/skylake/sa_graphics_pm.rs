// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native systemagent.c Gen9 graphics power management and display-audio setup.

use super::cpu::{Cpu, Family};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use fstart_arch::x86::cpu::msr::{MsrAccess, PLATFORM_INFO, PlatformInfo};
use fstart_core::mmio::{MmioReadOnly as RO, MmioReadWrite as RW};
use fstart_pci::EcamDevice;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
};

pub(crate) const MMIO_BYTES: usize = 0x20_0000;
register_bitfields![u32,
    WakeRequest [
        ENABLE OFFSET(0) NUMBITS(1) [],
        MASK OFFSET(16) NUMBITS(1) []
    ],
    WakeAck [ ACTIVE OFFSET(0) NUMBITS(1) [] ],
    Mailbox [
        COMMAND OFFSET(0) NUMBITS(8) [],
        BUSY OFFSET(31) NUMBITS(1) []
    ],
    Config0 [
        FEATURES OFFSET(1) NUMBITS(3) [],
        SAMPLE_PART OFFSET(30) NUMBITS(1) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    GatingFeatures [
        BIT20 OFFSET(20) NUMBITS(1) [],
        BIT25 OFFSET(25) NUMBITS(1) []
    ],
    GatingControl [
        FIELD24 OFFSET(24) NUMBITS(2) [],
        FIELD26 OFFSET(26) NUMBITS(2) []
    ],
    SpcControl [
        BIT24 OFFSET(24) NUMBITS(1) [],
        BIT26 OFFSET(26) NUMBITS(1) [],
        BIT31 OFFSET(31) NUMBITS(1) []
    ],
    CdclkControl [
        DECIMAL OFFSET(0) NUMBITS(11) [],
        FREQUENCY_MODE OFFSET(19) NUMBITS(1) [],
        FREQUENCY OFFSET(26) NUMBITS(2) []
    ],
    CdclkPll [
        ACK OFFSET(30) NUMBITS(1) [],
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    DisplayPowerWell [
        MISC_ACK OFFSET(0) NUMBITS(1) [],
        MISC_REQUEST OFFSET(1) NUMBITS(1) [],
        PG1_ACK OFFSET(28) NUMBITS(1) [],
        PG1_REQUEST OFFSET(29) NUMBITS(1) [],
        PG2_ACK OFFSET(30) NUMBITS(1) [],
        PG2_REQUEST OFFSET(31) NUMBITS(1) []
    ],
    DisplayFuse [
        PG2 OFFSET(25) NUMBITS(1) [],
        PG1 OFFSET(26) NUMBITS(1) [],
        PCH OFFSET(27) NUMBITS(1) []
    ],
    ResetHandshake [ ENABLE OFFSET(4) NUMBITS(1) [] ],
    // Display audio frequency control (GTTMMADR + 0x65900, 32-bit).
    DisplayAudio [
        CLOCK48 OFFSET(3) NUMBITS(1) [],
        CLOCK96 OFFSET(4) NUMBITS(1) [],
        TMODE OFFSET(15) NUMBITS(1) []
    ],
    DpllControl [ LINK_RATE OFFSET(1) NUMBITS(3) [] ]
];
register_structs! {
    SpcGroup {
        (0 => _reserved0),
        (0x08 => low: [RW<u32, SpcControl::Register>; 3]),
        (0x14 => _reserved1),
        (0x88 => high: [RW<u32, SpcControl::Register>; 3]),
        (0x94 => _reserved2),
        (0x100 => @END),
    },
    GtRegisters {
        (0 => _reserved0),
        (0x0d00 => config0: RW<u32, Config0::Register>),
        (0x0d04 => _reserved1),
        (0x0d40 => reg_d40: RW<u32>),
        (0x0d44 => _reserved2),
        (0x0d48 => reg_d48: RW<u32>),
        (0x0d4c => _reserved3),
        (0x0d84 => render_ack: RO<u32, WakeAck::Register>),
        (0x0d88 => media_ack: RO<u32, WakeAck::Register>),
        (0x0d8c => _reserved4),
        (0x0fdc => gating_control: RW<u32, GatingControl::Register>),
        (0x0fe0 => _reserved5),
        (0x2050 => render_idle: RW<u32>),
        (0x2054 => render_wake: RW<u32>),
        (0x2058 => _reserved6),
        (0x4090 => reg_4090: RW<u32>),
        (0x4094 => _reserved7),
        (0x9044 => reg_9044: RW<u32>),
        (0x9048 => _reserved8),
        (0x9120 => gating_features: RO<u32, GatingFeatures::Register>),
        (0x9124 => _reserved9),
        (0x9400 => reg_9400: RW<u32>),
        (0x9404 => reg_9404: RW<u32>),
        (0x9408 => reg_9408: RW<u32>),
        (0x940c => reg_940c: RW<u32>),
        (0x9410 => _reserved10),
        (0x9424 => reg_9424: RW<u32>),
        (0x9428 => _reserved11),
        (0xa000 => reg_a000: RW<u32>),
        (0xa004 => _reserved12),
        (0xa008 => reg_a008: RW<u32>),
        (0xa00c => reg_a00c: RW<u32>),
        (0xa010 => reg_a010: RW<u32>),
        (0xa014 => reg_a014: RW<u32>),
        (0xa018 => _reserved13),
        (0xa024 => reg_a024: RW<u32>),
        (0xa028 => _reserved14),
        (0xa02c => reg_a02c: RW<u32>),
        (0xa030 => reg_a030: RW<u32>),
        (0xa034 => _reserved15),
        (0xa068 => reg_a068: RW<u32>),
        (0xa06c => reg_a06c: RW<u32>),
        (0xa070 => reg_a070: RW<u32>),
        (0xa074 => _reserved16),
        (0xa090 => reg_a090: RW<u32>),
        (0xa094 => reg_a094: RW<u32>),
        (0xa098 => reg_a098: RW<u32>),
        (0xa09c => reg_a09c: RW<u32>),
        (0xa0a0 => _reserved17),
        (0xa0a8 => reg_a0a8: RW<u32>),
        (0xa0ac => reg_a0ac: RW<u32>),
        (0xa0b0 => reg_a0b0: RW<u32>),
        (0xa0b4 => _reserved18),
        (0xa0b8 => reg_a0b8: RW<u32>),
        (0xa0bc => _reserved19),
        (0xa168 => reg_a168: RW<u32>),
        (0xa16c => _reserved20),
        (0xa180 => reg_a180: RW<u32>),
        (0xa184 => _reserved21),
        (0xa188 => force_wake: RW<u32, WakeRequest::Register>),
        (0xa18c => _reserved22),
        (0xa194 => reg_a194: RW<u32>),
        (0xa198 => _reserved23),
        (0xa214 => reg_a214: RW<u32>),
        (0xa218 => reg_a218: RW<u32>),
        (0xa21c => _reserved24),
        (0xa248 => reg_a248: RW<u32>),
        (0xa24c => _reserved25),
        (0xa250 => reg_a250: RW<u32>),
        (0xa254 => _reserved26),
        (0xa25c => reg_a25c: RW<u32>),
        (0xa260 => _reserved27),
        (0xa270 => media_wake: RW<u32, WakeRequest::Register>),
        (0xa274 => _reserved28),
        (0xa278 => render_wake_request: RW<u32, WakeRequest::Register>),
        (0xa27c => _reserved29),
        (0xc3e4 => reg_c3e4: RW<u32>),
        (0xc3e8 => _reserved30),
        (0x12050 => video_idle: RW<u32>),
        (0x12054 => video_wake: RW<u32>),
        (0x12058 => _reserved31),
        (0x1a050 => enhance_idle: RW<u32>),
        (0x1a054 => enhance_wake: RW<u32>),
        (0x1a058 => _reserved32),
        (0x1c050 => video2_idle: RW<u32>),
        (0x1c054 => video2_wake: RW<u32>),
        (0x1c058 => _reserved33),
        (0x22050 => blitter_idle: RW<u32>),
        (0x22054 => blitter_wake: RW<u32>),
        (0x22058 => _reserved34),
        (0x24000 => spc: [SpcGroup; 8]),
        (0x24800 => _reserved35),
        (0x42000 => display_fuse: RO<u32, DisplayFuse::Register>),
        (0x42004 => _reserved36),
        (0x4402c => reg_4402c: RW<u32>),
        (0x44030 => _reserved37),
        (0x45400 => display_power: RW<u32, DisplayPowerWell::Register>),
        (0x45404 => _reserved38),
        (0x46000 => cdclk: RW<u32, CdclkControl::Register>),
        (0x46004 => _reserved39),
        (0x46010 => cdclk_pll: RW<u32, CdclkPll::Register>),
        (0x46014 => _reserved40),
        (0x46408 => reset_handshake: RW<u32, ResetHandshake::Register>),
        (0x4640c => _reserved41),
        (0x65900 => display_audio: RW<u32, DisplayAudio::Register>),
        (0x65904 => _reserved42),
        (0x6c058 => dpll: RO<u32, DpllControl::Register>),
        (0x6c05c => _reserved43),
        (0x130044 => force_ack: RO<u32, WakeAck::Register>),
        (0x130048 => _reserved44),
        (0x138124 => mailbox: RW<u32, Mailbox::Register>),
        (0x138128 => mailbox_data: RW<u32>),
        (0x13812c => mailbox_data_high: RW<u32>),
        (0x138130 => _reserved45),
        (0x200000 => @END),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Mapping,
    Device,
    BarChanged,
    RetainedMemory,
    Cpu,
    ForceWake,
    MediaWake,
    RenderWake,
    Mailbox,
    DisplayFuse,
    DisplayPower,
    DisplayPll,
}

pub struct Graphics<'a> {
    igd: EcamDevice,
    registers: &'a GtRegisters,
    physical: u64,
}
impl<'a> Graphics<'a> {
    /// # Safety
    /// Own the actual Gen9 IGD, its PCI configuration, and the first 2 MiB of
    /// its GTTMMADR mapped UC at virtual_base for 'a. No concurrent GPU/PCI/DMA
    /// owner may change this assignment. This does not initialize a framebuffer.
    pub unsafe fn at(
        igd: EcamDevice,
        virtual_base: usize,
        physical: u64,
        mapped_length: usize,
    ) -> Result<Self, Error> {
        if virtual_base == 0
            || virtual_base & 0xfff != 0
            || physical == 0
            || physical & 0xfff != 0
            || mapped_length < MMIO_BYTES
            || virtual_base.checked_add(MMIO_BYTES - 1).is_none()
        {
            return Err(Error::Mapping);
        }
        let result = Self {
            igd,
            registers: unsafe { &*(virtual_base as *const GtRegisters) },
            physical,
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), Error> {
        let p = &self.igd;
        if p.address().segment() != 0
            || p.bus() != 0
            || p.dev() != 2
            || p.func() != 0
            || p.vendor_id() != 0x8086
            || p.read8(0x0b) != 3
        {
            return Err(Error::Device);
        }
        if p.read32(0x10) & 7 != 4 || p.memory_bar(0x10) != Some(self.physical) {
            return Err(Error::BarChanged);
        }
        Ok(())
    }

    /// Native cold graphics-PM sequence. Restore PCI decode/aperture state on
    /// error, but do not claim to undo partial GPU power/lock transitions.
    pub fn configure_pm(
        &self,
        cpu: Cpu,
        mode: MemoryBootMode,
        msrs: &impl MsrAccess,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.validate()?;
        if mode != MemoryBootMode::Cold {
            return Err(Error::RetainedMemory);
        }
        if Cpu::from_signature(cpu.signature).map_err(|_| Error::Cpu)? != cpu {
            return Err(Error::Cpu);
        }
        let command = unsafe { self.igd.register::<u16, ()>(0x04) };
        let msac = unsafe { self.igd.register::<u8, ()>(0x62) };
        let old_command = command.get();
        let old_msac = msac.get();
        msac.set(old_msac & !0x1f);
        msac.set(msac.get() | 1);
        command.set(old_command | 6);
        let result = self.program_pm(cpu, msrs, delay);
        msac.set(old_msac);
        command.set(old_command);
        result
    }

    fn program_pm(
        &self,
        cpu: Cpu,
        msrs: &impl MsrAccess,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let r = self.registers;
        r.reg_d40.set(0x80000000);
        r.reg_d48.set(1);
        r.force_wake
            .write(WakeRequest::MASK::SET + WakeRequest::ENABLE::SET);
        wait(&r.force_ack, 1, 1, delay).map_err(|_| Error::ForceWake)?;
        r.config0.modify(Config0::FEATURES.val(7));
        r.reg_9044.set(r.reg_9044.get() | (1 << 30));
        r.reg_a250.set(0x1ff);
        r.reg_a25c.set(0x10);
        r.reg_a248.set(0x80000004);
        r.reg_a000.set(if cpu.stepping_id() <= 2 {
            0x703ff
        } else {
            0x70020
        });
        r.reg_a180.set(0xc5200000);
        r.reg_a194.set(r.reg_a194.get() | (1 << 5));
        r.reg_9424.set(0x7fd);
        r.reg_9400.set(0);
        r.reg_9404.set(0x40401000);
        r.reg_9408.set(0);
        r.reg_940c.set(0x02000001);
        r.reg_a008.set(0x03018000);
        r.reg_a00c.set(0x0c800000);
        r.media_wake
            .write(WakeRequest::MASK::SET + WakeRequest::ENABLE::SET);
        wait(&r.media_ack, 1, 1, delay).map_err(|_| Error::MediaWake)?;
        r.render_wake_request
            .write(WakeRequest::MASK::SET + WakeRequest::ENABLE::SET);
        wait(&r.render_ack, 1, 1, delay).map_err(|_| Error::RenderWake)?;
        r.reg_a214.set((r.reg_a214.get() & !1) | (1 << 31));
        r.reg_a218.set(r.reg_a218.get() & !1);
        // Unchanged opaque RC6 recipe from systemagent.c.
        for (register, keep, value) in [
            (&r.reg_a090, 0x7fffffff, 0),
            (&r.reg_a098, 0, 0x03e80000),
            (&r.reg_a09c, 0, 0x00280000),
            (&r.reg_a0a8, 0, 0x0001e848),
            (&r.reg_a0ac, 0, 0x19),
            (&r.render_wake, 0, 0xa),
            (&r.video_wake, 0, 0xa),
            (&r.blitter_wake, 0, 0xa),
            (&r.enhance_wake, 0, 0xa),
            (&r.video2_wake, 0, 0xa),
            (&r.render_idle, 0, 0x10000),
            (&r.video_idle, 0, 0x10000),
            (&r.blitter_idle, 0, 0x10000),
            (&r.enhance_idle, 0, 0x10000),
            (&r.video2_idle, 0, 0x10000),
            (&r.reg_c3e4, 0, 0xa),
            (&r.reg_a0b0, 0, 0),
            (&r.reg_a0b8, 0, 0xc350),
            (&r.reg_a010, 0, 0xf4240),
            (&r.reg_a014, 0, 0x12060000),
            (&r.reg_a02c, 0, 0xe808),
            (&r.reg_a030, 0, 0x3bd08),
            (&r.reg_a068, 0, 0x101d0),
            (&r.reg_a06c, 0, 0x55730),
            (&r.reg_a070, 0, 0xa),
            (&r.reg_a168, 0, 6),
            (&r.reg_a024, 0, 0x592),
        ] {
            register.set((register.get() & keep) | value);
        }
        if cpu.family == Family::Skylake {
            r.reg_4090.set(r.reg_4090.get() | (1 << 8));
        }
        r.reg_a090.set(0x88040000);
        wait(&r.mailbox, 1 << 31, 0, delay).map_err(|_| Error::Mailbox)?;
        r.mailbox_data.set(0);
        r.mailbox.set(0x80000004);
        wait(&r.mailbox, 1 << 31, 0, delay).map_err(|_| Error::Mailbox)?;
        r.reg_4402c.set(0x03000076);
        r.reg_a094.set(0x40000);
        r.media_wake
            .write(WakeRequest::MASK::SET + WakeRequest::ENABLE::CLEAR);
        wait(&r.media_ack, 1, 0, delay).map_err(|_| Error::MediaWake)?;
        r.render_wake_request
            .write(WakeRequest::MASK::SET + WakeRequest::ENABLE::CLEAR);
        wait(&r.render_ack, 1, 0, delay).map_err(|_| Error::RenderWake)?;
        r.force_wake
            .write(WakeRequest::MASK::SET + WakeRequest::ENABLE::CLEAR);
        wait(&r.force_ack, 1, 0, delay).map_err(|_| Error::ForceWake)?;
        let mut spc = SpcControl::BIT31::SET;
        if !r.gating_features.is_set(GatingFeatures::BIT25) {
            r.gating_control.modify(GatingControl::FIELD26.val(1));
            spc = SpcControl::BIT26::SET;
        }
        if r.gating_features.is_set(GatingFeatures::BIT20) {
            r.gating_control.modify(GatingControl::FIELD24.val(1));
            spc = SpcControl::BIT24::SET;
        }
        // Preserve the source order, including the sparse 0x240xx/0x245xx groups.
        for register in [
            &r.spc[6].low[0],
            &r.spc[6].low[1],
            &r.spc[6].low[2],
            &r.spc[6].high[0],
            &r.spc[6].high[1],
            &r.spc[6].high[2],
            &r.spc[7].low[0],
            &r.spc[7].low[1],
            &r.spc[7].low[2],
            &r.spc[7].high[0],
            &r.spc[7].high[1],
            &r.spc[7].high[2],
            &r.spc[0].low[0],
            &r.spc[0].high[0],
            &r.spc[0].high[1],
            &r.spc[0].high[2],
            &r.spc[1].low[0],
            &r.spc[1].low[1],
            &r.spc[1].low[2],
            &r.spc[1].high[0],
            &r.spc[1].high[1],
            &r.spc[1].high[2],
            &r.spc[4].low[0],
            &r.spc[4].low[1],
            &r.spc[4].low[2],
            &r.spc[4].high[0],
            &r.spc[4].high[1],
            &r.spc[4].high[2],
            &r.spc[5].low[0],
            &r.spc[5].low[1],
            &r.spc[5].low[2],
            &r.spc[2].low[0],
            &r.spc[2].low[1],
            &r.spc[2].low[2],
            &r.spc[2].high[0],
            &r.spc[2].high[1],
            &r.spc[2].high[2],
        ] {
            register.modify(spc);
        }
        let sample = msrs.reg(PLATFORM_INFO).read(PlatformInfo::SAMPLE_PART);
        r.config0
            .modify(Config0::SAMPLE_PART.val(sample as u32) + Config0::LOCK::SET);
        r.cdclk
            .modify(CdclkControl::DECIMAL.val(0x544) + CdclkControl::FREQUENCY.val(3));
        Ok(())
    }

    /// Native display power-up before HDA codec detection; no display modeset.
    pub fn init_display_audio(&self, delay: &mut impl DelayNs) -> Result<(), Error> {
        self.validate()?;
        let command = unsafe { self.igd.register::<u16, ()>(0x04) };
        let old = command.get();
        command.set(old | 6);
        let result = self.power_display_audio(delay);
        command.set(old);
        result
    }

    fn power_display_audio(&self, delay: &mut impl DelayNs) -> Result<(), Error> {
        let r = self.registers;
        r.reset_handshake.modify(ResetHandshake::ENABLE::SET);
        wait(&r.display_fuse, 1 << 27, 1 << 27, delay).map_err(|_| Error::DisplayFuse)?;
        r.display_power.modify(
            DisplayPowerWell::PG2_REQUEST::SET
                + DisplayPowerWell::PG1_REQUEST::SET
                + DisplayPowerWell::MISC_REQUEST::SET,
        );
        wait(
            &r.display_power,
            (1 << 30) | (1 << 28) | 1,
            (1 << 30) | (1 << 28) | 1,
            delay,
        )
        .map_err(|_| Error::DisplayPower)?;
        wait(
            &r.display_fuse,
            (1 << 26) | (1 << 25),
            (1 << 26) | (1 << 25),
            delay,
        )
        .map_err(|_| Error::DisplayFuse)?;
        let frequency = r.cdclk.read(CdclkControl::FREQUENCY);
        if matches!(r.dpll.read(DpllControl::LINK_RATE), 4 | 5) {
            r.cdclk.modify(CdclkControl::FREQUENCY_MODE::SET);
            r.cdclk_pll.modify(CdclkPll::ENABLE::SET);
            let lock = wait(&r.cdclk_pll, 1 << 30, 1 << 30, delay);
            r.cdclk.modify(CdclkControl::FREQUENCY.val(frequency));
            r.cdclk.modify(CdclkControl::FREQUENCY_MODE::CLEAR);
            lock.map_err(|_| Error::DisplayPll)?;
        } else {
            r.cdclk_pll.modify(CdclkPll::ENABLE::SET);
            wait(&r.cdclk_pll, 1 << 30, 1 << 30, delay).map_err(|_| Error::DisplayPll)?;
        }
        Ok(())
    }

    pub fn configure_audio_frequency(&self, mhz96: bool, tmode2t: bool) -> Result<(), Error> {
        self.validate()?;
        let command = unsafe { self.igd.register::<u16, ()>(0x04) };
        let old = command.get();
        command.set(old | 6);
        self.registers.display_audio.modify(
            DisplayAudio::CLOCK48.val(u32::from(!mhz96))
                + DisplayAudio::CLOCK96.val(u32::from(mhz96))
                + DisplayAudio::TMODE.val(u32::from(!mhz96 && !tmode2t)),
        );
        command.set(old);
        Ok(())
    }
}

fn wait<R: Readable<T = u32>>(
    register: &R,
    mask: u32,
    expected: u32,
    delay: &mut impl DelayNs,
) -> Result<(), ()> {
    for _ in 0..10_000 {
        if register.get() & mask == expected {
            return Ok(());
        }
        delay.delay_us(10);
    }
    Err(())
}
