// SPDX-License-Identifier: MIT OR Apache-2.0
//! Slippy's EC handoff and Peppy's sleep rails. The BSP supplies one byte
//! after installing this image, before running any payload. Never consult a
//! pointer or mutable configuration in OS RAM from SMM.
#![cfg(feature = "smm")]

use crate::slippy::{EC_POLICY, EC_SMI_GPI_MASK};
use fstart_driver_chromeec::handoff::{SmiInput, SmmState};
use fstart_platform_intel::{
    haswell::{PrepareError, SmmConfig},
    smm::SmmContext,
};

static STATE: SmmState = SmmState::new();

fn handler(_: &mut SmmContext<'_>, c: &SmmConfig) {
    // SAFETY: only the permanent SMM rendezvous owner calls this function.
    unsafe {
        c.dispatch(|p| {
            let hz = fstart_arch::x86::tsc_frequency_hz();
            STATE
                .dispatch(
                    &EC_POLICY,
                    EC_SMI_GPI_MASK,
                    SmiInput {
                        apm: p.apm,
                        acpi_enabled: p.acpi_enabled(),
                        requested_acpi_mode: p.requested_acpi_mode(),
                        gpi: p.gpi,
                        sleep: p.sleep,
                    },
                    crate::slippy::ec,
                    |us| fstart_arch::x86::udelay_tsc(us, hz),
                    || p.request_poweroff(),
                    || {
                        if let Some(gpio) = c.lp_gpio() {
                            gpio.set(46, false); // WLAN_DISABLE_L
                            gpio.set(45, false); // PP3300_CODEC_EN
                            gpio.set(59, false); // LTE_DISABLE_L
                        }
                    },
                    crate::slippy::post,
                )
                .map_err(|_| PrepareError::Cancelled)
        })
    };
}

fstart_platform_intel::smm::smm_bin!(handler: handler);
