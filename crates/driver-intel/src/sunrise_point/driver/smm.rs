// SPDX-License-Identifier: MIT OR Apache-2.0
//! Expose the actual bound PCH controller to the existing shared SMM installer;
//! the normal-stage owner supplies no alternate relocation/handler algorithm.
use super::{IntelSunrisePoint, smi};
use fstart_arch::x86::cpu::intel::smm::SmiControl;
impl IntelSunrisePoint {
    fn smi_controller(&self) -> &smi::Controller {
        self.smi
            .as_ref()
            .expect("SMM requires authenticated PCH post-memory initialization")
    }
}
impl SmiControl for IntelSunrisePoint {
    type EnableState = smi::EnableState;
    type HandlerConfig = smi::Config;
    fn smm_handler_config(&self) -> Self::HandlerConfig {
        self.smi_controller().smm_handler_config()
    }
    fn set_acpi_mode(&self, enabled: bool) {
        self.smi_controller().set_acpi_mode(enabled);
    }
    fn quiesce_for_relocation(&self) -> Self::EnableState {
        self.smi_controller().quiesce_for_relocation()
    }
    fn enable_permanent_smi(&self, previous: Self::EnableState) {
        self.smi_controller().enable_permanent_smi(previous);
    }
    fn lock_permanent_smi(&self) -> bool {
        self.smi_controller().lock_permanent_smi()
    }
}
