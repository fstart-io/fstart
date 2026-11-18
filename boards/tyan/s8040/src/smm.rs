// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM: the PSP's SMM info, its P2C requests and its EINJ service, and the
//! machine-check handler, which reports to the BMC's SEL over the
//! firmware's KCS channel ([`crate::bmc::sel`])
//! (`fstart_platform_amd::rs::smm`).

use fstart_platform_amd::board::Board as _;
use fstart_platform_amd::rs::smm::{self, Config};
use fstart_platform_amd::smm::SmmContext;

fn enter_cpu(ctx: &mut SmmContext<'_>, config: &Config) {
    smm::enter_cpu(ctx, config, &crate::Board::RAS);
}

fn smi_handler(ctx: &mut SmmContext<'_>, config: &Config) {
    smm::handle_with(ctx, config, &crate::Board::RAS, crate::bmc::sel);
}

fstart_platform_amd::smm::smm_bin!(handler: smi_handler, enter_cpu: enter_cpu);
