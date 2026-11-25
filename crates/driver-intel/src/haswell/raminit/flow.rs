//! Cold and cached task schedules and CAR-owned cache payload.
use super::{
    centering::{Eye, EyeMargins},
    configure_mc::{Config, TrainingState},
    ddr3::{ModeRegisters, ResetPin},
    memory_map::{MemoryMap, Reservations},
    save_restore::TrainingRegisters,
    spd::{Slot, Topology},
    testing_io::Stepping,
    timings::{Clock, Timings},
    *,
};
use crate::generic::training::{HardwareIdentity, identity};
use fstart_core::{memory_info::MemoryInfo, services::SmBus};
use fstart_pci::ecam::EcamDevice;
use sha2::{Digest, Sha256};
use zerocopy::{FromZeros, Immutable, IntoBytes, KnownLayout};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootMode {
    Cold,
    Fast,
    Warm,
    S3,
}
impl BootMode {
    fn destructive(self) -> bool {
        self != Self::S3
    }

    fn prepare_dram(
        self,
        m: MchBar,
        t: &Topology,
        mrs: &ModeRegisters,
        pin: &mut ResetPin<'_>,
        sync_io: impl FnOnce(MchBar) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if self.destructive() {
            sync_io(m)?;
            if ddr3::reset_and_program(m, t, mrs, false, pin)? != 0 {
                return Err(Error::Reut);
            }
            pretrain(m, t);
        }
        Ok(())
    }
}
/// Facts from the pre-memory Lynx Point and ME flows. `self_refresh` is
/// GEN_PMCON_2.DISB && MEM_SR sampled *before* clearing DISB.
#[derive(Clone, Copy)]
pub struct BootState {
    pub self_refresh: bool,
    pub s3resume: bool,
    pub cpu_replaced: bool,
}
impl BootState {
    pub fn from_pmcon(pmcon2: u16, s3resume: bool, cpu_replaced: bool) -> Self {
        Self {
            self_refresh: pmcon2 & 0xa0 == 0xa0,
            s3resume,
            cpu_replaced,
        }
    }
    pub fn select(self, valid_cache: bool, early_reset: bool) -> Result<BootMode, Error> {
        if self.s3resume {
            return if self.self_refresh && valid_cache && !self.cpu_replaced {
                Ok(BootMode::S3)
            } else {
                Err(Error::ResumeRequiresReset)
            };
        }
        if !valid_cache || self.cpu_replaced || self.self_refresh && early_reset {
            return Ok(BootMode::Cold);
        }
        Ok(if self.self_refresh {
            BootMode::Warm
        } else {
            BootMode::Fast
        })
    }
}
/// Inputs to the native algorithm. PEG, PCH USB routing and SPD strap
/// decoding are consumed by the platform, not the DDR3 controller.
pub struct Board<'a> {
    pub config: Config,
    pub reservations: Reservations,
    pub slots: [[Slot<'a>; 2]; 2],
}
impl Board<'_> {
    /// Key for fstart's existing training-data journal. Raw SPD is checked
    /// separately in the payload even when the same strap/address is reused.
    pub fn cache_key(&self, host: EcamDevice, cpuid: u32) -> [u8; 32] {
        let mut map = [0; 4];
        for (i, slot) in self.slots.iter().flatten().enumerate() {
            map[i] = match slot {
                Slot::Empty => 0,
                Slot::MemoryDown(_) => 0xff,
                Slot::SmBus(a) => *a,
            };
        }
        let key = identity(
            b"haswell-native-ddr3-v3",
            &map,
            0,
            0,
            HardwareIdentity {
                vendor_id: host.read16(0),
                device_id: host.read16(2),
                revision_id: host.read8(8),
                capabilities: [host.read32(0xe4), host.read32(0xe8)],
                cpu_signature: cpuid,
            },
        );
        let mut h = Sha256::new();
        h.update(key);
        h.update([
            self.config.ult.into(),
            self.config.dq_pins_interleaved.into(),
            self.config.double_refresh.into(),
            self.reservations.igd_primary.into(),
            u8::from(host.read32(0x54) & 16 != 0 || self.reservations.igd_primary),
        ]);
        for n in [
            self.reservations.me,
            self.reservations.tseg,
            self.reservations.dpr,
        ] {
            h.update(n.to_le_bytes());
        }
        if let Some(comp) = self.config.rcomp {
            h.update(comp.resistor);
            h.update(comp.target);
        }
        h.finalize().into()
    }
}
/// Physical channel, physical slot, SPD EEPROM byte.
pub type Spds = [[[u8; 256]; 2]; 2];
#[repr(C)]
#[derive(IntoBytes, Immutable, KnownLayout)]
pub(super) struct Frame {
    pub(super) magic: [u8; 4],
    pub(super) key: [u8; 32],
    pub(super) spd: Spds,
    pub(super) tck: u32,
    pub(super) regs: TrainingRegisters,
}
pub const CACHE_BYTES: usize = core::mem::size_of::<Frame>();
impl Frame {
    pub(super) const EMPTY: Self = Self {
        magic: [0; 4], // Keep Workspace::EMPTY eligible for CAR .bss.
        key: [0; 32],
        spd: [[[0; 256]; 2]; 2],
        tck: 0,
        regs: TrainingRegisters::EMPTY,
    };
    pub(super) fn accept<'a>(
        &self,
        input: &'a [u8],
        t: &Topology,
        cap_b: u32,
    ) -> Option<(Clock, &'a [u8])> {
        // A byte-slice payload from flash need not be word-aligned.
        let header = core::mem::offset_of!(Frame, tck);
        if input.len() != CACHE_BYTES || self.as_bytes()[..header] != input[..header] {
            return None;
        }
        let tck = u32::from_le_bytes(input[header..header + 4].try_into().ok()?);
        let c = Clock::candidates(t, cap_b).find(|c| c.tck == tck)?;
        Some((c, &input[header + 4..]))
    }
}
/// Allocate statically in CAR, not on the bootblock stack. Capture and
/// replay use the same backing array. No allocation or SPI writes in raminit.
pub struct Workspace {
    state: TrainingState,
    frame: Frame,
    margins: [EyeMargins; 3],
}
impl Workspace {
    pub const EMPTY: Self = Self {
        state: TrainingState::EMPTY,
        frame: Frame::EMPTY,
        margins: [[[[[0; 2]; 9]; 2]; 4]; 3],
    };
}
pub struct Initialized<'a> {
    pub mode: BootMode,
    /// DRAM was in self-refresh on entry but this schedule reset it.
    pub memory_lost: bool,
    pub map: MemoryMap,
    pub clock: Clock,
    pub timings: Timings,
    pub topology: Topology,
    pub spd: &'a Spds,
    pub save_data: &'a [u8],
    pub identity: [u8; 32],
    capid_a: u32,
    voltage_mv: u16,
}
impl Initialized<'_> {
    /// Type 16/17 inventory. The SPD is indexed by physical slot even when
    /// MAD_DIMM swaps the larger DIMM into logical slot A.
    pub fn memory_info(&self) -> MemoryInfo {
        use PCI_CAPID0_A as cap;
        let capid_a = self.capid_a;
        let channels = 2 - u32::from(cap::PDCD.is_set(capid_a));
        let capacity = spd::channel_capacity_mib(capid_a) * channels;
        let mut info = MemoryInfo::new(capacity);
        if self.topology.lanes == 9 {
            info.ecc = 5;
        }
        for ch in 0..2 {
            for slot in 0..2 {
                if let Some(d) = self.topology.dimms[ch][slot] {
                    let mut dev = fstart_driver_spd::ddr3::memory_device(
                        &d,
                        &self.spd[ch][slot],
                        ch as u8,
                        slot as u8,
                        self.clock.transfer_rate_mts(),
                    );
                    dev.voltage_mv = self.voltage_mv.into();
                    info.push(dev);
                } else if self.topology.wired[ch][slot] {
                    info.push(fstart_core::memory_info::MemoryDevice::empty(
                        ch as u8, slot as u8,
                    ));
                }
            }
        }
        info
    }
}
fn pretrain(m: MchBar, t: &Topology) {
    if t.lanes == 9 {
        for ch in 0..2 {
            m.setbits32(0x5004 + 4 * ch, MAD_DIMM::ECC_IO_ON::SET.value);
            fstart_arch::x86::udelay(4);
        }
    }
    wdb::setup(m, t);
}
macro_rules! task {
    ($name:literal,$expr:expr) => {{
        fstart_log::debug!("Haswell raminit: {}", $name);
        $expr.inspect_err(|_| fstart_log::error!("Haswell raminit failed: {}", $name))?
    }};
}

fn finish_replay(mode: BootMode, m: MchBar, t: &Topology, step: Stepping) -> Result<(), Error> {
    task!("EXIT_SR", ddr3::exit_selfrefresh(m, t));
    activate::normal_state(m, t);
    // Fast and warm reuse training, not contents or ECC parity.
    if mode.destructive() && t.lanes == 9 {
        task!("ECCSCRUB", memory_scrub::ecc_scrub(m, t, step));
    }
    Ok(())
}

/// Initialize the memory controller after the platform has brought up ME,
/// PCH, DMI (traditional only), ECAM and the BARs. Clear DISB before a cold
/// or promoted fast boot. On any S3 failure the caller must reset, never
/// retry with a destructive cold schedule. Publish `save_data` via the store
/// only after success and never on S3.
///
/// # Safety
/// Must run once on the BSP, from CAR, on a supported Haswell, with exclusive
/// access to the memory controller. The host bridge and its MCHBAR/GDXCBAR,
/// TXT public space and MSRs must be accessible. `cache` must come from a
/// trusted, integrity-checked journal record, not arbitrary caller data.
/// `pin` must match the actual DRAM reset wiring and `boot` the actual sleep
/// state. Only S3 preserves DRAM contents.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub unsafe fn raminit<'a>(
    m: MchBar,
    board: &Board<'_>,
    boot: BootState,
    bus: Option<&mut dyn SmBus>,
    mut pin: ResetPin<'_>,
    cache: Option<&[u8]>,
    workspace: &'a mut Workspace,
) -> Result<Initialized<'a>, Error> {
    let host = EcamDevice::new(0, 0, 0);
    let (cpu, _, ecx, _) = fstart_arch::x86::cpuid(1);
    let step = Stepping::from_cpuid(cpu).ok_or(Error::UnsupportedCpu)?;
    if board.config.ult != matches!(cpu, 0x40650 | 0x40651)
        || board.config.ult != matches!(pin, ResetPin::LynxPointLp(_))
    {
        return Err(Error::UnsupportedCpu);
    }
    let cap_a = host.read32(0xe4);
    let cap_b = host.read32(0xe8);
    info::capabilities(cap_a, cap_b);
    // Haswell TXT clear handshake before touching DRAM.
    if ecx & (1 << 6) != 0 {
        // SAFETY: caller guarantees TXT public space (0xfed3_0000) and TPM
        // locality 0 (0xfed4_0000) are mapped.
        let txt_pub = unsafe { fstart_core::mmio::MmioWindow::new(0xfed3_0000, 0x1000) };
        if txt_pub.read32(0x10) & 1 != 0 {
            let tpm = unsafe { fstart_core::mmio::MmioWindow::new(0xfed4_0000, 0x1000) };
            poll(|| tpm.read8(0) & 0x80 != 0, 2_000_000)?;
        }
    }
    unsafe { fstart_arch::x86::msr::wrmsr(0x2e6, 0) };
    let Workspace {
        state: s,
        frame,
        margins,
    } = workspace;
    s.zero();
    frame.magic = *b"HSW3";
    frame.key = board.cache_key(host, cpu);
    let t = task!(
        "PROCSPD",
        spd::collect(board.slots, bus, cap_a, board.config.ult, &mut frame.spd)
    );
    let saved = cache.and_then(|data| frame.accept(data, &t, cap_b));
    let early_reset = m.read32(0x5d10) == 0 && m.read32(0x5d14) == 0;
    let mut mode = boot.select(saved.is_some(), early_reset)?;
    let (c, tm, map) = loop {
        let restored = if mode != BootMode::Cold {
            let (c, regs) = saved.ok_or(Error::InvalidCache)?;
            frame.regs.as_mut_bytes().copy_from_slice(regs);
            Some(c)
        } else {
            None
        };
        let attempt = (|| {
            let c = task!("INITMPLL", timings::initialise_mpll(m, &t, cap_b, restored));
            let mut tm = Timings::from_spd(&t, c, cap_a);
            // The frame stores the clock, not native Rust struct padding.
            // All non-training timings are deterministic from validated SPD,
            // fuse and board inputs; reconstruct the 2x-refresh reduction too.
            task!(
                "CONFMC",
                configure_mc::configure(m, &t, c, &mut tm, board.config, s)
            );
            let map = task!("MEMMAP", unsafe {
                memory_map::configure(host, m, t.channel_size_mib.iter().sum(), board.reservations)
            });
            let mrs = ModeRegisters::derive(&t, tm, board.config.ult);
            task!(
                "JEDECINIT",
                mode.prepare_dram(m, &t, &mrs, &mut pin, ddr3::sync_io)
            );
            if mode == BootMode::Cold {
                task!("SOT", sense_amp::train(m, &t, s));
                task!("RCVET", receive_enable::train(m, &t, c, step, cpu, s));
                task!("RDMPRT", centering::train_mpr(m, &t, step, s));
                task!(
                    "JWRL",
                    write_leveling::train(m, &t, c, step, board.config.ult, &mrs, s)
                );
                for (i, eye) in [Eye::WriteTiming, Eye::ReadTiming, Eye::ReadVoltage]
                    .into_iter()
                    .enumerate()
                {
                    task!(
                        "1D centering",
                        centering::train_1d(m, &t, step, s, eye, &mut margins[i])
                    );
                }
                io_comp::optimise_comp(m, &t, s);
                activate::post_training(m, &t, tm);
                task!(
                    "LCT",
                    late_command::train(m, &t, step, tm.cmd, s, &mrs, &mut pin)
                );
                activate::activate(m, &t, c, tm, step, board.config.ult, s);
                frame.regs.capture(m);
                frame.tck = c.tck;
                task!("ALIASCHK", memory_scrub::alias_check(m, &t, step));
                task!("ECCSCRUB", memory_scrub::ecc_scrub(m, &t, step));
            } else {
                unsafe { frame.regs.restore(m) };
                frame.tck = c.tck;
                finish_replay(mode, m, &t, step)?;
            }
            Ok((c, tm, map))
        })();
        match attempt {
            Ok(result) => break result,
            // Unlike a completed rejection, a timed-out hardware operation
            // may still be running. Do not start a new schedule over it.
            Err(e @ (Error::PllBusy | Error::Timeout)) => return Err(e),
            Err(_) if mode == BootMode::Fast || mode == BootMode::Warm => {
                fstart_log::warn!("Haswell cache replay failed; retraining");
                s.zero();
                mode = BootMode::Cold;
            }
            Err(e) => return Err(e),
        }
    };
    task!("RAMINITEND", activate::done(m, host));
    info::report(m);
    Ok(Initialized {
        memory_lost: boot.self_refresh && mode.destructive(),
        mode,
        map,
        clock: c,
        timings: tm,
        topology: t,
        spd: &frame.spd,
        save_data: frame.as_bytes(),
        identity: frame.key,
        capid_a: cap_a,
        voltage_mv: if board.config.ult { 1350 } else { 1500 },
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resumes_never_fall_back_to_destructive_training() {
        for pm in [0, 0x20, 0x80] {
            assert_eq!(
                BootState::from_pmcon(pm, true, false).select(true, false),
                Err(Error::ResumeRequiresReset)
            );
        }
        let s3 = BootState::from_pmcon(0xa0, true, false);
        assert_eq!(s3.select(false, false), Err(Error::ResumeRequiresReset));
        assert_eq!(s3.select(true, false), Ok(BootMode::S3));
        assert_eq!(
            BootState {
                cpu_replaced: true,
                ..s3
            }
            .select(true, false),
            Err(Error::ResumeRequiresReset)
        );
        let warm = BootState {
            s3resume: false,
            ..s3
        };
        assert_eq!(warm.select(true, true), Ok(BootMode::Cold));
        assert_eq!(warm.select(true, false), Ok(BootMode::Warm));
        let cold = BootState {
            self_refresh: false,
            ..warm
        };
        assert_eq!(cold.select(true, false), Ok(BootMode::Fast));
        assert_eq!(cold.select(false, false), Ok(BootMode::Cold));
    }
    #[test]
    fn warm_resets_dimms_and_initializes_parity_but_s3_does_neither() {
        use crate::haswell::raminit::tests::{HYNIX, fixture, peppy};
        let mut d = fixture(HYNIX);
        d[8] |= 8;
        let crc = spd::crc(&d);
        d[126..128].copy_from_slice(&crc.to_le_bytes());
        let t = peppy([[Slot::MemoryDown(&d), Slot::Empty]; 2]);
        for mode in [BootMode::Warm, BootMode::S3] {
            let mut aperture = [0u64; crate::haswell::MCHBAR_SIZE / 8];
            let m = crate::haswell::raminit::tests::host_mch(&mut aperture);
            m.write32(0x4804, 3 << 16);
            let mut pins = [false; 3];
            let mut edges = 0;
            let mut pin = |asserted| {
                pins[edges] = asserted;
                edges += 1;
            };
            mode.prepare_dram(
                m,
                &t,
                &ModeRegisters::default(),
                &mut ResetPin::LynxPointLp(&mut pin),
                |_| Ok(()),
            )
            .unwrap();
            finish_replay(mode, m, &t, Stepping::C0).unwrap();
            if mode == BootMode::Warm {
                assert_eq!(&pins[..edges], &[false, true, false]);
                assert_eq!(m.read32(0x4040), 0x8092); // ECC scrub WDB mux
            } else {
                assert_eq!(edges, 0);
                assert_eq!(m.read32(0x4040), 0);
            }
        }
    }

    #[test]
    fn inventory_keeps_empty_wired_socket_but_not_unwired_slots() {
        use crate::haswell::raminit::tests::{HYNIX, fixture};
        use fstart_core::services::ServiceError;
        struct Absent;
        impl SmBus for Absent {
            fn read_byte(&mut self, _: u8, _: u8) -> Result<u8, ServiceError> {
                Err(ServiceError::NoDevice)
            }
            fn write_byte(&mut self, _: u8, _: u8, _: u8) -> Result<(), ServiceError> {
                unreachable!()
            }
        }
        let d = fixture(HYNIX);
        let mut raw = [[[0; 256]; 2]; 2];
        let topology = spd::collect(
            [[Slot::MemoryDown(&d), Slot::SmBus(0x51)], [Slot::Empty; 2]],
            Some(&mut Absent),
            0,
            true,
            &mut raw,
        )
        .unwrap();
        let clock = Clock::candidates(&topology, 0).next().unwrap();
        let data = Initialized {
            mode: BootMode::Cold,
            memory_lost: false,
            map: MemoryMap::compute(
                2048,
                Reservations {
                    tseg: 8,
                    dpr: 0,
                    me: 0,
                    igd_primary: false,
                },
                false,
            )
            .unwrap(),
            clock,
            timings: Timings::from_spd(&topology, clock, 0),
            topology,
            spd: &raw,
            save_data: &[],
            identity: [0; 32],
            capid_a: 0,
            voltage_mv: 1350,
        };
        let info = data.memory_info();
        let devices = info.devices();
        assert_eq!(devices.len(), 2);
        assert!(devices[0].is_populated());
        assert_eq!(
            (
                devices[1].channel,
                devices[1].slot,
                devices[1].is_populated()
            ),
            (0, 1, false)
        );
    }
    #[test]
    fn journal_payload_checks_version_spd_key_and_clock_before_replay() {
        use crate::haswell::raminit::tests::{HYNIX, fixture};
        let mut frame = Frame::EMPTY;
        let d = fixture(HYNIX);
        let t = spd::collect(
            [[Slot::MemoryDown(&d), Slot::Empty]; 2],
            None,
            0,
            true,
            &mut frame.spd,
        )
        .unwrap();
        frame.magic = *b"HSW2";
        frame.tck = 320;
        assert_eq!(CACHE_BYTES, 3560);
        let mut bytes = [0; CACHE_BYTES + 1];
        bytes[1..].copy_from_slice(frame.as_bytes());
        let data = &mut bytes[1..];
        let (c, regs) = frame.accept(data, &t, 0).unwrap();
        assert_eq!(c.mhz, 800);
        assert_eq!(regs.len(), 624 * 4);
        assert!(frame.accept(&data[1..], &t, 0).is_none());
        for off in [0, 4, 36, core::mem::offset_of!(Frame, tck)] {
            data[off] ^= 1;
            assert!(frame.accept(data, &t, 0).is_none());
            data[off] ^= 1;
        }
    }
}
