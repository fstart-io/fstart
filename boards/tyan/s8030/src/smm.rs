//! Native FCH dispatch and MCA observation on the S8030. RAS is platform
//! functionality selected by this board's policy, not a different board.

use fstart_platform_sp3::smm::{self, Policy, SmmConfig};
use fstart_platform_sp3::smm::ras::SelTransport;
use fstart_platform_sp3::smm_runtime::SmmContext;

const RAS: Policy = Policy {
    observe_mce: true,
    memory_einj: true,
    // Saved S8030 BMC image 2b09b92ee677b5d9: DTB at 0x39a200 enables
    // KCS2 (CA8/CAC) and its kcsbridged service. KCS3 (CA2/CA3) remains
    // the OS interface. The existing CA0..CAF LPC window decodes both.
    sel: Some(SelTransport {
        port: 0xca8,
        spacing: 4,
    }),
};

fn enter_cpu(ctx: &mut SmmContext<'_>, config: &SmmConfig) {
    smm::enter_cpu(ctx, config, &RAS);
}

fn smi_handler(ctx: &mut SmmContext<'_>, config: &SmmConfig) {
    smm::handle(ctx, config, &RAS);
}

fstart_platform_sp3::smm_runtime::smm_bin!(handler: smi_handler, enter_cpu: enter_cpu);
