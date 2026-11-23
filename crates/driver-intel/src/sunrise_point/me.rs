// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native me.c/finalize.c client-ME function lifecycle. No firmware download,
//! reset execution, or fabricated EOP/MBP success. One-shot mainstage owner.
use super::{
    pcr::{self, Pcr, Port},
    pmc::ExtendedReset,
};
use crate::{
    heci::Budgets,
    skylake::cse::{self, BiosPath, EndOfPostAction, FirmwareStatus, Hfs1, Hfs4, MmioCse},
};
use embedded_hal::delay::DelayNs;
use fstart_core::mmio::MmioRegister;
use fstart_pci::{EcamDevice, PCI_COMMAND_BITS};
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
register_bitfields![u32,
    Shadow [ DISABLE OFFSET(8) NUMBITS(1) [] ],
    IdleControl [
        IN_PROGRESS OFFSET(0) NUMBITS(1) [],
        IDLE OFFSET(2) NUMBITS(1) []
    ],
    Hfs3 [ SKU OFFSET(4) NUMBITS(3) [] ]
];
register_bitfields![u16, PowerState [ STATE OFFSET(0) NUMBITS(2) [] ]];
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub heci2: bool,
    /// Only the native no-MBP/error path uses this board setting. With a valid
    /// payload HECI3 visibility comes from actual integrated-touch capability.
    pub heci3_without_payload: bool,
    pub amt_sol: bool,
    pub heci1_visible_to_os: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    AlreadyAttempted,
    Device,
    Bar,
    Timeout,
    Readback,
    Policy,
    GlobalResetRequired,
    Cse(cse::Error),
    Pcr(pcr::Error),
}
impl From<cse::Error> for Error {
    fn from(e: cse::Error) -> Self {
        Self::Cse(e)
    }
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    New,
    Running,
    Initialized,
    Ready,
    Finished,
    Failed,
}
pub struct NativeMe {
    policy: Policy,
    phase: Phase,
    status: Option<FirmwareStatus>,
    visible: bool,
}
impl NativeMe {
    pub const fn new(policy: Policy) -> Self {
        Self {
            policy,
            phase: Phase::New,
            status: None,
            visible: true,
        }
    }
    /// # Safety
    /// Actual PCH-H PCR/HECI/PMC resources are exclusively owned and mapped UC.
    /// An authenticated cold DID predecessor exists; scratch is unique DRAM,
    /// never CAR or a DMA-owned payload. Do not retry a partially failed owner.
    pub unsafe fn initialize(
        &mut self,
        pcr: &Pcr,
        scratch: &mut [u8],
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        if self.phase != Phase::New {
            return Err(Error::AlreadyAttempted);
        }
        self.phase = Phase::Running;
        let result = self.initialize_inner(pcr, scratch, delay);
        self.phase = if result.is_ok() {
            Phase::Initialized
        } else {
            Phase::Failed
        };
        result
    }
    fn initialize_inner(
        &mut self,
        pcr: &Pcr,
        scratch: &mut [u8],
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let heci = EcamDevice::new(0, 0x16, 0);
        let initial = FirmwareStatus::read(&heci)?;
        let payload = if initial.allows(cse::Command::BiosPayload) {
            set_idle(&heci, false, delay)?;
            let mut service = unsafe { MmioCse::bind(heci, Budgets::SOURCE) }?;
            service.initialize(delay)?;
            Some(service.read_bios_payload(scratch, delay)?)
        } else {
            None
        }; // explicit source-filtered no-MBP path, not a success payload
        let status = FirmwareStatus::read(&heci)?;
        let hfs = LocalRegisterCopy::<u32, Hfs1::Register>::new(status.hfs1);
        let sku = LocalRegisterCopy::<u32, Hfs3::Register>::new(heci.read32(0x60)).read(Hfs3::SKU);
        let mut enabled = [
            true,
            self.policy.heci2,
            false,
            self.policy.amt_sol && sku == 3,
            payload.map_or(self.policy.heci3_without_payload, |p| {
                p.firmware_capabilities
                    .is_some_and(|caps| caps & (1 << 4) != 0)
            }),
        ];
        let error = hfs.read(Hfs1::ERROR);
        if error == 3 {
            enabled[1] = false;
            enabled[4] = false;
            enabled[3] = false;
        } else if error != 0 {
            enabled[1] = false;
            enabled[3] = false;
        }
        if hfs.is_set(Hfs1::FPT_BAD) || hfs.read(Hfs1::WORKING) == 2 {
            enabled[1] = false;
            enabled[3] = false;
        }
        if status.hfs4 & Hfs4::ENFORCEMENT::SET.value != 0 {
            enabled.fill(false);
        }
        match hfs.read(Hfs1::MODE) {
            2 | 4 => enabled.fill(false),
            3 | 5 => {
                enabled[1] = false;
                enabled[3] = false;
                enabled[4] = false;
            }
            _ => {}
        }
        for (function, enable) in enabled.into_iter().enumerate() {
            configure_function(pcr, function as u8, enable, delay)?;
        }
        self.visible = enabled[0];
        self.status = Some(status); // actual snapshot before any function hide
        Ok(())
    }
    /// Cold ready-to-boot. Only validated action 0 or the native explicitly
    /// disabled-function paths complete. A reset request NEVER becomes Ready.
    pub fn ready_to_boot(&mut self, pcr: &Pcr, delay: &mut impl DelayNs) -> Result<(), Error> {
        if self.phase != Phase::Initialized {
            return Err(Error::AlreadyAttempted);
        }
        self.phase = Phase::Running;
        let result = self.ready_inner(pcr, delay);
        self.phase = if result.is_ok() {
            Phase::Ready
        } else {
            Phase::Failed
        };
        result
    }
    fn ready_inner(&mut self, pcr: &Pcr, delay: &mut impl DelayNs) -> Result<(), Error> {
        let heci = EcamDevice::new(0, 0x16, 0);
        let status = if self.visible {
            FirmwareStatus::read(&heci)?
        } else {
            self.status.ok_or(Error::Device)?
        };
        if self.visible {
            match status.bios_path() {
                BiosPath::Normal | BiosPath::Error | BiosPath::Recovery => {
                    set_idle(&heci, false, delay)?;
                    let mut service = unsafe { MmioCse::bind(heci, Budgets::SOURCE) }?;
                    service.initialize(delay)?;
                    if service.end_of_post(delay)? != EndOfPostAction::Continue {
                        return Err(Error::GlobalResetRequired);
                    }
                }
                BiosPath::TemporaryDisable
                | BiosPath::SecurityOverrideMei
                | BiosPath::SecurityOverrideJumper => {
                    configure_function(pcr, 0, false, delay)?;
                    self.visible = false;
                }
                // Native clients normally never reach this point in update,
                // SPS, debug or enforcement. Do not invent an EOP waiver.
                _ => return Err(Error::Policy),
            }
        }
        let pmc = EcamDevice::new(0, 0x1f, 2);
        if pmc.vendor_id() != 0x8086 {
            return Err(Error::Device);
        }
        let hfs = LocalRegisterCopy::<u32, Hfs1::Register>::new(status.hfs1);
        let lock = matches!(hfs.read(Hfs1::MODE), 0 | 3) && !hfs.is_set(Hfs1::MANUFACTURING);
        let reset = unsafe { pmc.register::<u32, ExtendedReset::Register>(0xac) };
        if reset.is_set(ExtendedReset::CF9_LOCK) && reset.is_set(ExtendedReset::GLOBAL_RESET) {
            return Err(Error::Readback);
        }
        reset.modify(ExtendedReset::GLOBAL_RESET::CLEAR);
        if lock {
            reset.modify(ExtendedReset::CF9_LOCK::SET);
        }
        if reset.is_set(ExtendedReset::GLOBAL_RESET)
            || lock && !reset.is_set(ExtendedReset::CF9_LOCK)
        {
            return Err(Error::Readback);
        }
        self.status = Some(status);
        Ok(())
    }
    pub fn finish(&mut self, pcr: &Pcr, delay: &mut impl DelayNs) -> Result<(), Error> {
        if self.phase != Phase::Ready {
            return Err(Error::AlreadyAttempted);
        }
        self.phase = Phase::Running;
        let result = (|| {
            for function in [0, 1, 4] {
                let device = EcamDevice::new(0, 0x16, function);
                if device.is_present() {
                    set_idle(&device, true, delay)?;
                }
            }
            if !self.policy.heci1_visible_to_os && self.visible {
                configure_function(pcr, 0, false, delay)?;
                self.visible = false;
            }
            Ok(())
        })();
        self.phase = if result.is_ok() {
            Phase::Finished
        } else {
            Phase::Failed
        };
        result
    }
}
fn shadow(pcr: &Pcr, function: u8, disable: bool) -> Result<(), Error> {
    let base = match function {
        0 => 0x600,
        1 => 0x400,
        2 => 0x800,
        3 => 0x700,
        4 => 0x300,
        _ => return Err(Error::Device),
    };
    let register = pcr.register::<Shadow::Register>(Port::Psf1, base + 0x1c)?;
    register.modify(Shadow::DISABLE.val(u32::from(disable)));
    if register.is_set(Shadow::DISABLE) != disable {
        return Err(Error::Readback);
    }
    Ok(())
}
fn configure_function(
    pcr: &Pcr,
    function: u8,
    enable: bool,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    if enable {
        shadow(pcr, function, false)?;
    }
    let device = EcamDevice::new(0, 0x16, function);
    if device.is_present() {
        if matches!(function, 0 | 1 | 4) {
            // The first HECI page is already reserved; other disabled functions
            // may have no BAR and therefore no reachable D0i3 controller.
            if device.memory_bar(0x10).is_some_and(|b| b != 0) {
                set_idle(&device, !enable, delay)?;
            }
        } else if !enable {
            unsafe { device.register::<u16, PowerState::Register>(0x54) }
                .write(PowerState::STATE.val(3));
        }
    } else if enable {
        return Err(Error::Device);
    }
    if !enable {
        shadow(pcr, function, true)?;
    }
    Ok(())
}
fn wait_idle(
    control: &MmioRegister<u32, IdleControl::Register>,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    for ms in 0..=5000 {
        if !control.is_set(IdleControl::IN_PROGRESS) {
            return Ok(());
        }
        if ms == 5000 {
            break;
        }
        delay.delay_ms(1);
    }
    Err(Error::Timeout)
}
fn set_idle(device: &EcamDevice, idle: bool, delay: &mut impl DelayNs) -> Result<(), Error> {
    if device.bus() != 0
        || device.dev() != 0x16
        || !matches!(device.func(), 0 | 1 | 4)
        || device.vendor_id() != 0x8086
    {
        return Err(Error::Device);
    }
    // D0i3 entry is defined only for MSI delivery, as in native me.c.
    if idle && device.read32(0xa0) != 0 {
        return Ok(());
    }
    let base = device
        .memory_bar(0x10)
        .filter(|b| *b != 0 && *b < 1u64 << 32 && b & 0xfff == 0)
        .ok_or(Error::Bar)?;
    let command = unsafe { device.register::<u16, PCI_COMMAND_BITS::Register>(4) };
    let saved = command.get();
    let decode = PCI_COMMAND_BITS::MEMORY_SPACE::SET.value;
    let bus_master = PCI_COMMAND_BITS::BUS_MASTER::SET.value;
    let enabled = saved | decode | if idle { bus_master } else { 0 };
    command.set(enabled);
    let result = (|| {
        if command.get() & (decode | bus_master) != enabled & (decode | bus_master) {
            return Err(Error::Readback);
        }
        let control =
            unsafe { MmioRegister::<u32, IdleControl::Register>::new(base as usize + 0x800) };
        let current = control.extract();
        if current.get() == u32::MAX {
            return Err(Error::Readback);
        }
        // Native me_clear_d0i3 leaves an already-awake HECI alone. Do not
        // require CIP to settle for a power transition we did not request.
        if current.is_set(IdleControl::IDLE) != idle {
            wait_idle(&control, delay)?;
            control.modify(IdleControl::IDLE.val(u32::from(idle)));
            wait_idle(&control, delay)?;
            if control.is_set(IdleControl::IDLE) != idle {
                return Err(Error::Readback);
            }
        }
        Ok(())
    })();
    command.set(if idle && result.is_ok() {
        (saved | bus_master) & !decode
    } else {
        saved
    });
    result
}
