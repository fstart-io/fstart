// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SoC on the flow: its silicon steps at the flow's phases and the
//! socket's services.
use super::silicon::Silicon;
use super::{Naples, State};
use crate::flow::{Ctx, Mapped, Reserved, Silicon as Up, Tables};
use crate::soc::{Entered, Init, Roots};
use crate::sp3::flow::step;
use crate::sp3::silicon::{Identity, steps};
use crate::sp3::soc as sp3;
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_arch::x86::mp::MpHandle;
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::ccx::{init::Threads, mp::Fam17hCpu};
use fstart_driver_amd::df::v2::Df2;
use fstart_driver_amd::psp;
use fstart_pci::PciEcam;

impl Init for Naples {
    const NAME: &'static str = "naples";

    type Config = sp3::Config;
    type Identity = Identity;
    type Hw = sp3::Hw;
    /// The DXIO engines of the board's lanes.
    type Lanes<'a> = &'a sp3::Engines;
    type Runtime = ();
    type FchUp = State;
    type Trained = State;
    type Chipset = State;
    type Cpu = Fam17hCpu<Self, Threads>;
    type State = State;

    /// ECAM, then the reset tasks of socket 0's FCH.
    fn enter(config: &sp3::Config, _: &NativeMsr) -> Entered<Self> {
        sp3::enter(config)
    }

    fn identify(hw: &sp3::Hw, _: &sp3::Config, _: Option<Tsc>) -> Result<Identity, &'static str> {
        sp3::check_identity(hw)
    }

    fn pci_segment(hw: &sp3::Hw) -> PciRootInfo {
        hw.pci_root
    }

    fn psp(hw: &sp3::Hw) -> psp::Variant {
        hw.identity.silicon().into()
    }

    /// The fabric, NBIO, the SMU and the secondary FCHs: everything up to
    /// DXIO. The board reads its detect pads and programs its GPIO table
    /// next.
    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<State, &'static str> {
        let mut soc = sp3::fabric::<Df2, Self>(ctx)?;
        step(0x21, "DF init");
        Silicon::df_init(&mut soc)?;
        step(0x30, "NBIO early");
        let mut si = Silicon::nbio_early(&mut soc)?;
        step(0x31, "SMU");
        Silicon::smu_init(&mut soc, &mut si)?;
        step(0x32, "FCH secondaries, USB");
        Silicon::fch_secondaries(&mut soc, &si)?;
        Ok(State::new(soc, si))
    }

    fn train_dxio(
        _: &Ctx<'_, Self>,
        mut state: State,
        engines: &sp3::Engines,
    ) -> Result<State, &'static str> {
        step(0x38, "DXIO");
        Silicon::dxio(&mut state.soc, &state.si, engines)?;
        Ok(state)
    }

    fn init_chipset(_: &Ctx<'_, Self>, state: State, (): ()) -> Result<State, &'static str> {
        step(0x40, "FCH env");
        steps::fch_env(&state.soc);
        Silicon::fch_env(&state.soc, &state.si)?;
        Ok(state)
    }

    fn cpu_driver(
        _: &Ctx<'_, Self>,
        state: &mut State,
        microcode: Option<&'static [u8]>,
    ) -> Result<Self::Cpu, &'static str> {
        step(0x50, "MP and CCX");
        let threads = Silicon::cpu_driver(&mut state.soc, &mut state.si, microcode)?;
        Ok(Fam17hCpu::multi_socket(
            Self::NAME,
            threads,
            fstart_driver_amd::ccx::init::thread_init,
        ))
    }

    fn threads(_: &Ctx<'_, Self>, state: &State) -> Result<u16, &'static str> {
        Ok(Silicon::threads(&state.si))
    }

    fn release_aps(_: &Ctx<'_, Self>, state: &State, aps: u16) -> Result<(), u32> {
        Silicon::release_aps(&state.soc, &state.si, aps)
    }

    fn cpu_errors(cpu: &Self::Cpu) -> u32 {
        cpu.errors()
    }

    fn cpus_up(
        _: &Ctx<'_, Self>,
        mut state: State,
        mp: &MpHandle,
        apic_ids: &[u32],
    ) -> Result<State, &'static str> {
        Silicon::cpus_up(&mut state.soc, &mut state.si, mp, apic_ids)?;
        Ok(state)
    }

    fn before_enumeration(_: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        step(0x60, "PCI enumeration");
        Silicon::pci_pre(&mut state.soc, &mut state.si)
    }

    fn roots(_: &Ctx<'_, Self>, state: &State) -> Option<Roots> {
        sp3::roots(state)
    }

    fn after_resources(_: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        Silicon::pci_post(&state.soc, &state.si)
    }

    fn ready_to_boot(_: &Ctx<'_, Self>, _: &mut State, _: &PciEcam) -> Result<(), &'static str> {
        Ok(())
    }

    /// ReadyToBoot (SOC_DF §3.7, PSP_APOB §3.4, FCH_SP3 §2.3): the FCH late
    /// tasks, the DF's extended config off, PSP LockDFReg, and BootDone
    /// last, before the APs are parked.
    fn finalize(_: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        step(0x80, "ReadyToBoot");
        steps::ready_to_boot(&state.soc, || Silicon::fch_late(&state.soc, &state.si))
    }

    /// BootDone is the last command of [`Init::finalize`].
    fn boot_done(_: &Ctx<'_, Self>, _: &mut State) -> Result<(), &'static str> {
        Ok(())
    }
}

impl crate::flow::Soc for Naples {
    const TSEG_SIZE: u32 = crate::sp3::smm::TSEG_SIZE as u32;
    type Flow = sp3::Flow;

    fn map(
        hw: &mut sp3::Hw,
        config: &mut sp3::Config,
        board: &crate::board::Facts,
        msr: &NativeMsr,
        _: Tsc,
    ) -> Result<Mapped<Self>, &'static str> {
        sp3::map(hw, config, board, msr)
    }

    /// EINJ's scratch pages, selected with the live routing map.
    fn smm_memory(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<Reserved, &'static str> {
        sp3::smm_memory(ctx.memory.e820, state)
    }

    fn install_smm(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        mp: &MpHandle,
        flow: &sp3::Flow,
    ) -> Result<(), &'static str> {
        let usb = (
            Silicon::usb_buses(&state.soc, &state.si)?,
            Silicon::usb_bus_count(&state.soc),
        );
        sp3::install_smm(ctx.board, state, mp, flow, usb)
    }

    fn tables(
        ctx: &Ctx<'_, Self>,
        silicon: &mut Up<Self>,
        pci: &PciEcam,
        flow: &sp3::Flow,
        smbios: &fstart_acpi::smbios::SmbiosIdentity<'_>,
        acpi: crate::acpi::Board,
    ) -> Tables {
        let numa = Silicon::numa(&silicon.state.soc, &silicon.state.si);
        let nbios = silicon.state.si.nbios.clone();
        sp3::tables(ctx.board, silicon, (numa, &nbios), pci, flow, smbios, acpi)
    }

    fn launch(
        handoff: fstart_payload::Handoff<'_>,
        _: &Ctx<'_, Self>,
        state: &State,
        flow: &sp3::Flow,
        options: &fstart_options::Report,
    ) -> ! {
        sp3::launch(handoff, state, flow, options)
    }
}
