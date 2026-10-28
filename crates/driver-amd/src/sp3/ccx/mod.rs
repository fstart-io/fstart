//! Core complex (CCX_SP3): the MSR table engine and the Zen / Zen 2 / Zen 3
//! tables, logical IDs, thread topology, AP release, APIC mode, downcore,
//! microcode, MCA, power management, name string, encryption and errata.
//!
//! Hardware access goes through [`MsrAccess`] (fstart-arch; `NativeMsr` on
//! the CPU, `MsrSpace` in tests), [`crate::smn::Smn`] and
//! [`crate::smu::SmuMessages`]. Per-thread code runs on the thread it
//! configures.
//!
//! # Sequence (CCX_SP3 §2, donor order)
//!
//! BSP, cold boot:
//! 1. [`x2apic::decide`] from the thread count (APOB 3/3 maps on Rome/Milan,
//!    THREAD_CONFIGURATION per die on Naples); on Milan with
//!    `downcore_to_7`, add [`downcore::CoreMode::SEVEN_PER_CCD`].
//! 2. [`downcore::plan`] / [`downcore::commit`]; warm reset if anything
//!    was written.
//! 3. [`Ccx::new`], then [`Ccx::run_tables`] on the BSP; [`brand::read`] +
//!    [`brand::write`].
//! 4. [`microcode::select`] + [`Ccx::load_microcode`]; [`Ccx::early`];
//!    [`power::wdt_setting`] (DF::DfGlobalCtrl); [`power::read_cac_weights`];
//!    Milan with SNP: [`security::rmp_plan`], allocate, program.
//! 5. For each [`release::sp3_order`] / [`release::naples_order`] entry:
//!    [`release::launch`], then wait (bounded) for the AP to check in. Each
//!    AP runs [`Ccx::early`] then [`Ccx::ap_init`].
//! 6. BSP: [`mca::init`], [`power::write_cac_weights`] ([`security::set_smee`] before the APs start).
//! 7. Every thread (MP callback): [`Ccx::all_threads`].
//!
//! The scope of the steps (BSP, every thread, primary threads) is handled
//! inside the functions from the [`topology::Thread`].

pub mod brand;
pub mod downcore;
pub mod errata;
pub mod logical;
pub mod mca;
pub mod power;
pub mod ras;
pub mod release;
pub mod security;
pub mod table;
pub mod topology;
pub mod x2apic;

#[cfg(feature = "naples")]
pub mod zen1;
#[cfg(feature = "rome")]
pub mod zen2;
#[cfg(feature = "milan")]
pub mod zen3;

use fstart_arch::x86::cpu::msr::MsrAccess;

use crate::ccx::microcode::{self, Load, Patch};
use crate::sp3::soc::{Generation, Identity, Revision};
use logical::LogicalId;
use power::{CstatePolicy, Wdt};
use security::{RmpPlan, SnpPolicy};
use table::{PlatformFlags, Record, TablePolicy};
use topology::Thread;
use x2apic::{ApicMode, ApicModePolicy};

/// Board/user policy for the CCX. `Default` is the donors' default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub apic: ApicModePolicy,
    pub table: TablePolicy,
    pub downcore: downcore::DowncorePolicy,
    pub cstates: CstatePolicy,
    /// Core performance boost.
    pub boost: bool,
    /// CPU watchdog request; `None` leaves it off.
    pub wdt: Option<Wdt>,
    /// Rome/Milan CpuWdtCfg severity; `None` keeps the reset value.
    pub wdt_severity: Option<u8>,
    /// SYSCFG.SMEE. Off: the donors differ (Naples never, Rome always,
    /// Milan per PCD) and nothing here supports SME yet.
    pub smee: bool,
    /// Milan SEV-SNP RMP; off.
    pub snp: Option<SnpPolicy>,
    pub zen3: errata::Zen3Controls,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            apic: ApicModePolicy::Auto,
            table: TablePolicy::default(),
            downcore: downcore::DowncorePolicy::default(),
            cstates: CstatePolicy::default(),
            boost: true,
            wdt: Some(Wdt::DEFAULT),
            wdt_severity: None,
            smee: false,
            snp: None,
            zen3: errata::Zen3Controls::default(),
        }
    }
}

/// Why the CCX cannot be set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CcxError {
    /// The generation's feature is not built.
    NotBuilt(Generation),
}

/// The per-boot CCX state every thread uses: the identified CPU, its
/// logical IDs, the platform flags and the table. `Copy`, so it can be
/// handed to the APs.
#[derive(Debug, Clone, Copy)]
pub struct Ccx {
    pub revision: Revision,
    /// CPUID Fn0000_0001_EAX.
    pub signature: u32,
    pub ids: LogicalId,
    pub apic: ApicMode,
    pub flags: PlatformFlags,
    table: &'static [Record],
}

/// What the BSP prepared for the APs' start routine.
#[derive(Clone, Copy)]
pub struct ApInputs<'a> {
    pub microcode: Option<Patch<'a>>,
    /// From [`power::wdt_setting`].
    pub wdt: Option<Wdt>,
    pub wdt_severity: Option<u8>,
    pub cac_weights: &'a [u64],
    /// Milan SNP: RMP base and plan.
    pub rmp: Option<(u64, RmpPlan)>,
}

impl Ccx {
    pub fn new(identity: Identity, apic: ApicMode, policy: TablePolicy) -> Result<Self, CcxError> {
        let generation = identity.generation();
        let table = table::records(generation).ok_or(CcxError::NotBuilt(generation))?;
        Ok(Self {
            revision: identity.revision,
            signature: identity.signature,
            ids: LogicalId::of(identity.revision),
            apic,
            flags: PlatformFlags::sp3(generation, apic, policy),
            table,
        })
    }

    pub const fn generation(&self) -> Generation {
        self.revision.generation()
    }

    /// Phase-1 tables on the executing thread (BSP early, APs at start).
    pub fn run_tables<M: MsrAccess + ?Sized>(&self, cpu: &M, thread: &Thread) {
        table::apply(cpu, self.table, thread, &self.ids, self.flags);
    }

    /// The BSP-early step ([`errata::early`]).
    pub fn early<M: MsrAccess + ?Sized>(&self, cpu: &M, thread: &Thread) {
        errata::early(cpu, thread, &self.ids);
    }

    /// Load `patch` if the thread is the primary thread of its core, and
    /// leave its address for the PSP's reload on S3 resume.
    ///
    /// # Safety
    /// As [`Patch::apply`]: the patch's bytes are identity mapped.
    pub unsafe fn load_microcode<M: MsrAccess + ?Sized>(
        &self,
        cpu: &M,
        thread: &Thread,
        patch: &Patch<'_>,
    ) -> Result<(), microcode::Error> {
        if !thread.primary() {
            return Ok(());
        }
        let load = Load {
            record_for_s3: true,
            ..Load::default()
        };
        // SAFETY: the caller's contract.
        unsafe { patch.apply(cpu, patch.address(), self.signature, load) }.map(|_| ())
    }

    /// The AP start routine (ZP `0x1240`, SSP `0x1324`, GN `0x15b4`): WDT
    /// (Naples/Rome), tables, microcode, RMP (Milan), MCA, CAC weights.
    ///
    /// # Safety
    /// As [`Self::load_microcode`] for `inputs.microcode`.
    pub unsafe fn ap_init<M: MsrAccess + ?Sized>(
        &self,
        cpu: &M,
        thread: &Thread,
        inputs: &ApInputs<'_>,
    ) -> Result<(), microcode::Error> {
        let primary = thread.primary();
        if primary && self.generation() != Generation::Milan {
            power::init_wdt(cpu, inputs.wdt, inputs.wdt_severity);
        }
        self.run_tables(cpu, thread);
        if let Some(p) = &inputs.microcode {
            // SAFETY: the caller's contract.
            unsafe { self.load_microcode(cpu, thread, p) }?;
        }
        if let Some((base, plan)) = &inputs.rmp {
            security::program_rmp(cpu, *base, plan);
        }
        mca::init(cpu, thread);
        if primary && !inputs.cac_weights.is_empty() {
            power::write_cac_weights(cpu, inputs.cac_weights);
        }
        Ok(())
    }

    /// The DXE all-thread step (every thread, after MP init): C-states, P0
    /// rewrite and boost, TW_CFG/Milan controls, CPU WDT (primary threads).
    pub fn all_threads<M: MsrAccess + ?Sized>(
        &self,
        cpu: &M,
        thread: &Thread,
        policy: &Policy,
        wdt: Option<Wdt>,
    ) {
        power::init_cstates(cpu, thread, &policy.cstates);
        power::init_pstate(cpu, policy.boost);
        errata::all_threads(cpu, thread, &self.ids, &policy.zen3);
        if thread.primary() {
            power::init_wdt(cpu, wdt, policy.wdt_severity);
        }
    }
}

#[cfg(all(test, feature = "rome"))]
mod tests {
    use super::*;
    use crate::sp3::soc::{PACKAGE_SP3, identify};
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    #[test]
    fn rome_ap_start() {
        let id = identify(0x0083_0F10, u32::from(PACKAGE_SP3) << 28).unwrap();
        let ccx = Ccx::new(id, ApicMode::XApic, TablePolicy::default()).unwrap();
        let cpu = MsrSpace::new([(0x179, 23)]);
        let thread = Thread::new(Generation::Rome, 0x10, true, false);
        let inputs = ApInputs {
            microcode: None,
            wdt: Some(Wdt::DEFAULT),
            wdt_severity: None,
            cac_weights: &[1, 2],
            rmp: None,
        };
        ccx.early(&cpu, &thread);
        // SAFETY: no microcode.
        unsafe { ccx.ap_init(&cpu, &thread, &inputs) }.unwrap();
        ccx.all_threads(&cpu, &thread, &Policy::default(), inputs.wdt);
        let cpu = cpu.into_inner();
        assert_eq!(cpu.writes[0].0, 0xc001_0074, "WDT first");
        assert_eq!(
            cpu.get(0xc001_1004) & 1 << 53,
            0,
            "xAPIC hides CPUID x2APIC"
        );
        assert_eq!(cpu.get(0xc001_0419), 0x3c0);
        assert_eq!(cpu.get(0xc001_0296), 0x48_4848);
        assert_eq!(cpu.get(0xc001_1093), 0x71_5870, "CCX scope ran");
    }
}
