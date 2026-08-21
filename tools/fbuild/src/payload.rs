use clap::ValueEnum;
use fstart_core::{BoardConfig, Compression, FdtSource, PayloadConfig, PayloadKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PayloadChoice {
    Uefi,
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
            Self::Linux => "linux",
            Self::Fit => "fit",
            Self::Shell => "shell",
            Self::Elf => "elf",
            Self::Halt => "halt",
        }
    }

    pub const fn kind(self) -> Option<PayloadKind> {
        match self {
            Self::Uefi => Some(PayloadKind::UefiPayload),
            Self::Linux => Some(PayloadKind::LinuxBoot),
            Self::Fit => Some(PayloadKind::FitImage),
            Self::Shell => Some(PayloadKind::Shell),
            Self::Elf => Some(PayloadKind::CustomElf),
            Self::Halt => None,
        }
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
        PayloadChoice::Uefi => matches!(
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
