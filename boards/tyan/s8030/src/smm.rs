//! SMM on the S8030: the FCH's dispatch, Rome's EINJ and the machine-check
//! handler, which reports to the BMC's SEL over the firmware's KCS
//! channel ([`crate::bmc::sel`]). RAS is platform functionality selected
//! by this board's policy, not a different board.

use fstart_platform_amd::board::Board as _;
use fstart_platform_amd::smm::{SmmConfig, SmmContext};
use fstart_platform_amd::sp3::smm::{self, Policy, Sp3};

const POLICY: Policy = Policy {
    ras: crate::Board::RAS,
    memory_einj: true,
};

fn enter_cpu(ctx: &mut SmmContext<'_>, config: &SmmConfig<Sp3>) {
    smm::enter_cpu(ctx, config, &POLICY);
}

fn smi_handler(ctx: &mut SmmContext<'_>, config: &SmmConfig<Sp3>) {
    smm::handle(ctx, config, &POLICY, crate::bmc::sel);
}

fstart_platform_amd::smm::smm_bin!(handler: smi_handler, enter_cpu: enter_cpu);
