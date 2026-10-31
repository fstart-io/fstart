//! OS-first SMM dispatch; no board-specific RAS or SEL transport.

use fstart_platform_sp3::smm::{self, Policy, SmmConfig};
use fstart_platform_sp3::smm_runtime::SmmContext;

fn smi_handler(ctx: &mut SmmContext<'_>, config: &SmmConfig) {
    smm::handle(ctx, config, &Policy::OS_FIRST);
}

fstart_platform_sp3::smm_runtime::smm_bin!(handler: smi_handler);
