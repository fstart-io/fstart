//! OS-first SMM dispatch; no board-specific RAS or SEL transport.

use fstart_platform_amd::smm::{SmmConfig, SmmContext};
use fstart_platform_amd::sp3::smm::{self, Policy, Sp3};

fn smi_handler(ctx: &mut SmmContext<'_>, config: &SmmConfig<Sp3>) {
    smm::handle(ctx, config, &Policy::OS_FIRST);
}

fstart_platform_amd::smm::smm_bin!(handler: smi_handler);
