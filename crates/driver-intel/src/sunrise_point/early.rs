// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native romstage_native.c PCH/CPU pre-memory sequence and saved wake state.

use super::{
    hsio, lpc,
    pcr::{self, Pcr, Port},
    pmc::{self, MemoryBootMode, PmcRegisters},
};
use crate::{
    skylake::cse::MmioCse,
    southbridge::pmio_ich::{PM_CONTROL, PM_STATUS},
};
use embedded_hal::delay::DelayNs;
use fstart_core::{
    indexed::Indexed,
    mmio::{MmioReadOnly as RO, MmioReadWrite as RW, MmioRegister},
    pio::PioRegister,
};
use fstart_driver_rtc::{self as rtc, RtcControl, RtcFrequency};
use fstart_pci::EcamDevice;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
};

pub const SPI_BASE: usize = 0xfe01_0000;
const TRACE_BASE: usize = 0xfe10_0000;
register_bitfields![u8,
    DmiMaster [
        DMIC16 OFFSET(0) NUMBITS(1) [],
        DMIC17 OFFSET(1) NUMBITS(1) []
    ],
    PostedWrite [ ENABLE OFFSET(0) NUMBITS(1) [] ],
    Port80Route [ PCIE OFFSET(3) NUMBITS(1) [] ],
    StrapLock [ LOCK OFFSET(0) NUMBITS(1) [] ],
    CommandByte [ MEMORY OFFSET(1) NUMBITS(1) [] ]
];
register_bitfields![u16,
    MasterMessage [ ENABLE OFFSET(0) NUMBITS(1) [] ]
];
register_bitfields![u32,
    MicrocodeRegion [ ENABLE OFFSET(0) NUMBITS(1) [] ],
    ResetControl [ GLOBAL_CF9 OFFSET(20) NUMBITS(1) [] ],
    PsfHide [ HIDE OFFSET(0) NUMBITS(1) [] ],
    PsfFunction [ DISABLE OFFSET(8) NUMBITS(1) [] ],
    DciPower [
        PMC_RE OFFSET(0) NUMBITS(1) [],
        I3 OFFSET(1) NUMBITS(1) [],
        D3HOT OFFSET(2) NUMBITS(1) [],
        HAE OFFSET(5) NUMBITS(1) []
    ],
    DciClock [ FIELD OFFSET(16) NUMBITS(4) [] ],
    DciControl [
        LOCK OFFSET(0) NUMBITS(1) [],
        ENABLE OFFSET(4) NUMBITS(1) [],
        DETECTED OFFSET(9) NUMBITS(2) []
    ],
    FiaPll [ LOCK OFFSET(31) NUMBITS(1) [] ],
    TraceScratch [ STT_CONNECTED OFFSET(24) NUMBITS(1) [] ],
    TraceTsu [ CTC_RESYNC OFFSET(0) NUMBITS(1) [] ],
    HswPower [
        GATE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ]
];
register_structs! {
    TraceRegisters {
        (0 => _reserved0),
        (0xe0 => scratch: RO<u32, TraceScratch::Register>),
        (0xe4 => _reserved1),
        (0x2000 => tsu: RW<u32, TraceTsu::Register>),
        (0x2004 => @END),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Decode,
    RetainedMemory,
    DciLocked,
    Readback,
    Pcr(pcr::Error),
    Pmc(pmc::DecodeError),
    Lpc(lpc::Error),
    Hsio(hsio::Error),
}
impl From<pcr::Error> for Error {
    fn from(error: pcr::Error) -> Self {
        Self::Pcr(error)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PowerState {
    pm1_status: u16,
    pm1_control: u32,
    reset: pmc::ResetState,
}
impl PowerState {
    pub fn mode(self) -> MemoryBootMode {
        let status = LocalRegisterCopy::<u16, PM_STATUS::Register>::new(self.pm1_status);
        let control = LocalRegisterCopy::<u32, PM_CONTROL::Register>::new(self.pm1_control);
        MemoryBootMode::from_power_state(
            self.reset.gen_pmcon_a,
            status.is_set(PM_STATUS::WAKE) && control.read(PM_CONTROL::SLEEP_TYPE) == 5,
        )
    }
    pub fn reset_state(self) -> pmc::ResetState {
        self.reset
    }
}

/// Capture before any PM1 or reset-status acknowledgement, including before
/// SPI write-enable setup. This creates observations, not a memory-ready flag.
pub fn capture_power_state(pmc: &PmcRegisters) -> Result<PowerState, Error> {
    let device = EcamDevice::new(0, 0x1f, 2);
    pmc::require_decodes(&device).map_err(Error::Pmc)?;
    Ok(PowerState {
        pm1_status: PioRegister::<u16, PM_STATUS::Register>::new(pmc::ACPI_BASE).get(),
        pm1_control: PioRegister::<u32, PM_CONTROL::Register>::new(pmc::ACPI_BASE + 4).get(),
        reset: pmc.capture_reset_state(&device).map_err(Error::Pmc)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMode {
    Absent,
    Disabled,
    ExistingDebugger,
}

/// Produced only by the actual native sequence, not by a caller-set ready bit.
#[derive(Debug, Clone, Copy)]
pub struct PreMemory {
    power: PowerState,
    hsio: hsio::PreMemory,
    trace: TraceMode,
}
impl PreMemory {
    pub fn power(self) -> PowerState {
        self.power
    }
    pub fn hsio(self) -> hsio::PreMemory {
        self.hsio
    }
    pub fn trace(self) -> TraceMode {
        self.trace
    }
}

/// # Safety
/// BSP-only with interrupts/APs/DMA quiesced and the native ECAM, PCR, PWRM,
/// SPI and Trace Hub windows mapped UC and exclusively owned. Initial LPC/PMC,
/// SPI and SMBus decodes must have completed. Capture must precede ANY other
/// clearing of PM1 WAK_STS/SLP_TYP. Own alternate CMOS index/data ports 74h/75h.
/// CSE must be the actual exclusively owned HECI1 transport on this silicon.
/// Any error after the first write is fatal partial setup, never a retry token.
pub unsafe fn pre_memory(
    pcr: &Pcr,
    pmc: &PmcRegisters,
    cse: &mut MmioCse,
    dci_enabled: bool,
    delay: &mut impl DelayNs,
) -> Result<PreMemory, Error> {
    let lpc = EcamDevice::new(0, 0x1f, 0);
    let silicon = hsio::Silicon::read(&lpc).map_err(Error::Hsio)?;
    let pmc_dev = EcamDevice::new(0, 0x1f, 2);
    let p2sb = EcamDevice::new(0, 0x1f, 1);
    let spi = EcamDevice::new(0, 0x1f, 5);
    let smbus = EcamDevice::new(0, 0x1f, 4);
    if p2sb.vendor_id() != 0x8086
        || p2sb.memory_bar(0x10) != Some(u64::from(pcr::DEFAULT_BASE))
        || p2sb.read16(0x04) & 2 == 0
        || spi.vendor_id() != 0x8086
        || spi.memory_bar(0x10) != Some(SPI_BASE as u64)
        || spi.read16(0x04) & 2 == 0
        || smbus.vendor_id() != 0x8086
    {
        return Err(Error::Decode);
    }
    let status = PioRegister::<u16, PM_STATUS::Register>::new(pmc::ACPI_BASE);
    let control = PioRegister::<u32, PM_CONTROL::Register>::new(pmc::ACPI_BASE + 4);
    let power = capture_power_state(pmc)?;
    if power.mode() != MemoryBootMode::Cold {
        return Err(Error::RetainedMemory);
    }
    let dci = pcr.register::<DciControl::Register>(Port::Dci, 0x04)?;
    if dci.is_set(DciControl::LOCK) && dci.is_set(DciControl::ENABLE) != dci_enabled {
        return Err(Error::DciLocked);
    }
    let trace_device = EcamDevice::new(0, 0x1f, 7);
    if trace_device.vendor_id() != 0xffff
        && (trace_device.vendor_id() != 0x8086
            || trace_device.read16(0x04) & 2 != 0
                && trace_device.memory_bar(0x10) != Some(TRACE_BASE as u64))
    {
        return Err(Error::Decode);
    }

    // Save wake information above before acknowledging it, as pmc_fill_power_state.
    status.write(PM_STATUS::WAKE::SET);
    control.modify(PM_CONTROL::SLEEP_TYPE.val(0));
    pcr.register16::<MasterMessage::Register>(Port::Itss, 0x3334)?
        .write(MasterMessage::ENABLE::SET);
    pcr.register8::<DmiMaster::Register>(Port::Dmi, 0x2236)?
        .modify(DmiMaster::DMIC16::SET + DmiMaster::DMIC17::SET);
    pcr.register::<MicrocodeRegion::Register>(Port::Dmi, 0x2748)?
        .modify(MicrocodeRegion::ENABLE::CLEAR);
    for index in 0..8u16 {
        let value = if index == 5 {
            0x0000f000
        } else if index == 6 {
            0x10
        } else {
            0
        };
        unsafe { p2sb.register::<u32, ()>(0x80 + index * 4) }.set(value);
    }
    unsafe { p2sb.register::<u8, PostedWrite::Register>(0xf4) }.modify(PostedWrite::ENABLE::SET);
    let gen_b = LocalRegisterCopy::<u32, pmc::GenPmconB::Register>::new(power.reset.gen_pmcon_b);
    if gen_b.is_set(pmc::GenPmconB::RTC_BATTERY_DEAD) {
        let index = PioRegister::<u8>::new(0x74);
        let data = PioRegister::<u8>::new(0x75);
        let frequency =
            Indexed::<_, _, RtcFrequency::Register>::new(index, data, rtc::reg::FREQ_SELECT);
        let rtc_control =
            Indexed::<_, _, RtcControl::Register>::new(index, data, rtc::reg::CONTROL);
        frequency.modify(RtcFrequency::DIVIDER.val(6));
        rtc_control.modify(RtcControl::SET::SET);
        frequency.modify(RtcFrequency::DIVIDER.val(2));
        rtc_control.modify(RtcControl::SET::CLEAR);
    }
    pmc.power_reset_status.set(1 << 4); // W1C: never echo unrelated PRSTS.
    unsafe { pmc_dev.register::<u32, ResetControl::Register>(0xac) }
        .modify(ResetControl::GLOBAL_CF9::CLEAR);
    unsafe { smbus.register::<u32, ()>(0x64) }.set(0x0a0a0000);
    lpc::open_io_window(&lpc, pcr, 0x80, 0x10).map_err(Error::Lpc)?;
    pcr.register::<PsfHide::Register>(Port::Psf3, 0x238)?
        .modify(PsfHide::HIDE::SET);
    // Native GbE completion-tracking transaction is a byte, not a dword RMW.
    let (port, offset) = if silicon.kind() == super::dmi::Kind::H {
        (Port::Psf3, 0x4020)
    } else {
        (Port::Psf1, 0x403c)
    };
    let gbe = pcr.register8::<()>(port, offset)?;
    gbe.set(gbe.get() & !0x20);
    let hsio = hsio::pre_memory(&lpc, pcr, cse, power.mode(), delay).map_err(Error::Hsio)?;
    pcr.register::<DciPower::Register>(Port::Dci, 0x30)?.modify(
        DciPower::D3HOT::CLEAR + DciPower::I3::CLEAR + DciPower::PMC_RE::CLEAR + DciPower::HAE::SET,
    );
    pcr.register::<DciClock::Register>(Port::Dci, 0x08)?
        .modify(DciClock::FIELD.val(0));
    let detected = !dci_enabled && dci.read(DciControl::DETECTED) != 0;
    dci.modify(DciControl::ENABLE.val(u32::from(dci_enabled)));
    dci.modify(DciControl::LOCK::SET);
    if !dci.is_set(DciControl::LOCK) || dci.is_set(DciControl::ENABLE) != dci_enabled {
        return Err(Error::Readback);
    }
    if dci_enabled || detected {
        pmc.hsw_power_gate.set(1 << 31);
    }
    // SAFETY: fixed mapped MTB and its configuration were preflighted above.
    let trace = unsafe { disable_trace_hub(pcr, pmc, &trace_device) }?;
    pcr.register8::<Port80Route::Register>(Port::Dmi, 0x274d)?
        .modify(Port80Route::PCIE::CLEAR);
    let pll = pcr.register::<FiaPll::Register>(Port::Fia, 0x20)?;
    pll.modify(FiaPll::LOCK::SET);
    if !pll.is_set(FiaPll::LOCK) {
        return Err(Error::Readback);
    }
    if power.pm1_status & PM_STATUS::WAKE::SET.value == 0 {
        let low_control = PioRegister::<u16>::new(pmc::ACPI_BASE + 4);
        let mut value =
            LocalRegisterCopy::<u32, PM_CONTROL::Register>::new(u32::from(low_control.get()));
        value.modify(PM_CONTROL::SLEEP_TYPE::S5);
        low_control.set(value.get() as u16);
    }
    let straps = unsafe { MmioRegister::<u8, StrapLock::Register>::new(SPI_BASE + 0xf0) };
    straps.modify(StrapLock::LOCK::SET);
    if !straps.is_set(StrapLock::LOCK) {
        return Err(Error::Readback);
    }
    Ok(PreMemory { power, hsio, trace })
}

unsafe fn disable_trace_hub(
    pcr: &Pcr,
    pmc: &PmcRegisters,
    device: &EcamDevice,
) -> Result<TraceMode, Error> {
    let power_byte =
        unsafe { MmioRegister::<u8>::new(core::ptr::addr_of!(pmc.hsw_power_gate) as usize) };
    power_byte.set(0);
    let _ = power_byte.get();
    if device.vendor_id() == 0xffff {
        return Ok(TraceMode::Absent);
    }
    if device.vendor_id() != 0x8086 {
        return Err(Error::Decode);
    }
    let command = unsafe { device.register::<u8, CommandByte::Register>(0x04) };
    let registers = unsafe { &*(TRACE_BASE as *const TraceRegisters) };
    if !command.is_set(CommandByte::MEMORY) {
        device.write32(0x10, TRACE_BASE as u32);
        device.write32(0x14, 0);
        command.modify(CommandByte::MEMORY::SET);
        if !registers.scratch.is_set(TraceScratch::STT_CONNECTED) {
            command.set(0);
            device.write32(0x10, 0);
        }
    }
    if command.is_set(CommandByte::MEMORY) {
        registers.tsu.modify(TraceTsu::CTC_RESYNC::SET);
        if pmc.hsw_power_gate.get() & (1 << 31) == 0 {
            pmc.hsw_power_gate.set(1 << 31);
        }
        return Ok(TraceMode::ExistingDebugger);
    }
    // Native temporarily opens the MTB even on the disable path for CTC resync.
    device.write32(0x10, TRACE_BASE as u32);
    device.write32(0x14, 0);
    command.modify(CommandByte::MEMORY::SET);
    registers.tsu.modify(TraceTsu::CTC_RESYNC::SET);
    command.set(0);
    device.write32(0x10, 0);
    pcr.register::<PsfHide::Register>(Port::Psf3, 0x338)?
        .modify(PsfHide::HIDE::SET);
    pcr.register::<PsfFunction::Register>(Port::Psf3, 0x31c)?
        .modify(PsfFunction::DISABLE::SET);
    if pmc.hsw_power_gate.get() & (1 << 31) == 0 {
        pmc.hsw_power_gate
            .set(pmc.hsw_power_gate.get() | (1 << 31) | 1);
    }
    Ok(TraceMode::Disabled)
}
