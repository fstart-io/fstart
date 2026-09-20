use clap::{Args, ValueEnum};
use fstart_core::{BoardConfig, Compression, FdtSource, PayloadConfig, PayloadKind};
use fstart_image_build::plan::BuildSelection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PayloadChoice {
    Uefi,
    UefiUi,
    UefiBasic,
    Linux,
    Fit,
    Shell,
    Elf,
    Halt,
}

impl PayloadChoice {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uefi => "uefi",
            Self::UefiUi => "uefi-ui",
            Self::UefiBasic => "uefi-basic",
            Self::Linux => "linux",
            Self::Fit => "fit",
            Self::Shell => "shell",
            Self::Elf => "elf",
            Self::Halt => "halt",
        }
    }

    pub const fn kind(self) -> Option<PayloadKind> {
        match self {
            Self::Uefi | Self::UefiUi | Self::UefiBasic => Some(PayloadKind::UefiPayload),
            Self::Linux => Some(PayloadKind::LinuxBoot),
            Self::Fit => Some(PayloadKind::FitImage),
            Self::Shell => Some(PayloadKind::Shell),
            Self::Elf => Some(PayloadKind::CustomElf),
            Self::Halt => None,
        }
    }
}

/// Direct x86 Linux launch policy for Intel boards.
///
/// Every field is optional: omitted addresses fall back to the family
/// defaults (`X86_LINUX_DEFAULT_KERNEL_LOAD_ADDR` /
/// `X86_LINUX_DEFAULT_ZERO_PAGE_ADDR`) and an omitted command line means no
/// command line. There is deliberately no implicit serial-console default —
/// pass `--linux-bootargs` explicitly.
#[derive(Debug, Clone, Default, Args)]
pub struct X86LinuxArgs {
    /// Physical address at which the bzImage protected-mode payload is loaded.
    #[arg(long, value_parser = parse_u64)]
    pub linux_kernel_load_addr: Option<u64>,
    /// Physical address used for the Linux boot-parameter zero page.
    #[arg(long, value_parser = parse_u64)]
    pub linux_zero_page_addr: Option<u64>,
    /// Command line passed by the direct x86 Linux launcher.
    #[arg(long)]
    pub linux_bootargs: Option<String>,
    /// Dump BSP MTRRs and control registers immediately before Linux handoff.
    #[arg(long, default_value_t = false)]
    pub linux_print_mtrrs: bool,
}

impl X86LinuxArgs {
    pub fn selection(&self, payload: Option<PayloadChoice>) -> BuildSelection {
        BuildSelection {
            payload: payload.map(|choice| choice.as_str().to_owned()),
            x86_linux_kernel_load_addr: self.linux_kernel_load_addr,
            x86_linux_zero_page_addr: self.linux_zero_page_addr,
            x86_linux_bootargs: self.linux_bootargs.clone(),
            x86_linux_print_mtrrs: self.linux_print_mtrrs,
        }
    }

    pub fn append_cli_args(&self, args: &mut Vec<String>) {
        if let Some(value) = self.linux_kernel_load_addr {
            args.extend(["--linux-kernel-load-addr".into(), format!("{value:#x}")]);
        }
        if let Some(value) = self.linux_zero_page_addr {
            args.extend(["--linux-zero-page-addr".into(), format!("{value:#x}")]);
        }
        if let Some(value) = &self.linux_bootargs {
            args.extend(["--linux-bootargs".into(), value.clone()]);
        }
        if self.linux_print_mtrrs {
            args.push("--linux-print-mtrrs".into());
        }
    }
}

fn parse_u64(value: &str) -> Result<u64, String> {
    let value = value.replace('_', "");
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).map_err(|error| error.to_string())
    } else {
        value
            .parse()
            .map_err(|error: std::num::ParseIntError| error.to_string())
    }
}

pub fn apply_payload_override(
    config: &mut BoardConfig,
    choice: Option<PayloadChoice>,
) -> Result<(), String> {
    validate_choice(config, choice)?;
    apply_to_payload(&mut config.payload, choice);
    Ok(())
}

fn validate_choice(config: &BoardConfig, choice: Option<PayloadChoice>) -> Result<(), String> {
    let Some(choice) = choice else {
        return Ok(());
    };
    let has_payload_stage = match &config.stages {
        fstart_core::StageLayout::Monolithic(stage) => stage.build.payload,
        fstart_core::StageLayout::MultiStage(stages) => {
            stages.iter().any(|stage| stage.build.payload)
        }
    };
    let supported = match choice {
        PayloadChoice::Halt => true,
        PayloadChoice::Linux => config
            .payload
            .as_ref()
            .is_some_and(|payload| payload.kind == PayloadKind::LinuxBoot),
        PayloadChoice::Uefi | PayloadChoice::UefiUi | PayloadChoice::UefiBasic => matches!(
            config.platform,
            fstart_core::Platform::X86_64
                | fstart_core::Platform::Aarch64
                | fstart_core::Platform::Riscv64
        ),
        PayloadChoice::Fit | PayloadChoice::Shell | PayloadChoice::Elf => false,
    };
    if has_payload_stage && supported {
        Ok(())
    } else {
        Err(format!(
            "payload '{}' is not supported by {}'s build plan",
            choice.as_str(),
            config.name
        ))
    }
}

fn apply_to_payload(payload: &mut Option<PayloadConfig>, choice: Option<PayloadChoice>) {
    let Some(choice) = choice else {
        return;
    };

    *payload = choice.kind().map(|kind| match payload.take() {
        Some(mut config) if config.kind == kind => {
            config.kind = kind;
            config
        }
        Some(config) => PayloadConfig {
            firmware: config.firmware,
            ..empty_payload_config(kind)
        },
        None => empty_payload_config(kind),
    });
}

fn empty_payload_config(kind: PayloadKind) -> PayloadConfig {
    PayloadConfig {
        kind,
        kernel_file: None,
        kernel_load_addr: None,
        x86_zero_page_addr: None,
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: None,
        print_x86_mtrrs: false,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

#[cfg(test)]
mod tests {
    use fstart_core::{FirmwareConfig, FirmwareKind, hstr};

    use super::*;

    #[test]
    fn cli_payload_overrides_board_default() {
        let mut payload = Some(empty_payload_config(PayloadKind::LinuxBoot));
        payload.as_mut().expect("linux payload").firmware = Some(FirmwareConfig {
            kind: FirmwareKind::ArmTrustedFirmware,
            file: hstr("bl31.bin"),
            load_addr: 0x0e09_0000,
        });

        apply_to_payload(&mut payload, Some(PayloadChoice::Uefi));
        let payload_config = payload.as_ref().expect("uefi payload");
        assert_eq!(payload_config.kind, PayloadKind::UefiPayload);
        assert!(payload_config.kernel_file.is_none());
        assert!(payload_config.firmware.is_some());

        apply_to_payload(&mut payload, Some(PayloadChoice::Halt));
        assert!(payload.is_none());
    }
}
