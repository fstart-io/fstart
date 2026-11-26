// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM: SP7 PSP `SmmInfo` / `RasEinj` / P2C handler and machine-check
//! reporting to the AST2700 BMC's SEL over KCS ([`crate::sel_if`]).

use fstart_platform_amd::board::Board as _;
use fstart_platform_amd::smm::SmmContext;
use fstart_platform_amd::sp7::smm::{self, Config};

fn enter_cpu(ctx: &mut SmmContext<'_>, config: &Config) {
    smm::enter_cpu(ctx, config, &crate::Board::RAS);
}

fn smi_handler(ctx: &mut SmmContext<'_>, config: &Config) {
    smm::handle_with(ctx, config, &crate::Board::RAS, |event, timer| {
        crate::sel_if(config.kcs_decoded, event, timer)
    });
}

fstart_platform_amd::smm::smm_bin!(handler: smi_handler, enter_cpu: enter_cpu);
