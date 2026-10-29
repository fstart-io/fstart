use clap::{Args, ValueEnum};
use fstart_image_build::plan::{BuildSelection, CompilerFileSelection};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PayloadChoice {
    Uefi,
    UefiUi,
    UefiBasic,
    Linux,
    /// coreboot payload ELF from `--kernel`, e.g. SeaBIOS.
    Coreboot,
    Halt,
}

/// Build-time log verbosity, compiled into every stage as `FSTART_LOG_LEVEL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

impl PayloadChoice {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uefi => "uefi",
            Self::UefiUi => "uefi-ui",
            Self::UefiBasic => "uefi-basic",
            Self::Linux => "linux",
            Self::Coreboot => "coreboot",
            Self::Halt => "halt",
        }
    }
}

/// Build-time firmware policy selected from the fbuild CLI.
///
/// The SMBIOS date defaults to the current UTC date. Direct x86 Linux fields
/// are optional: the omitted kernel address falls back to platform policy and
/// an omitted command line means no command line. The zero-page address is
/// fixed by Intel platform policy rather than exposed as an unchecked physical
/// memory override.
#[derive(Debug, Clone, Args)]
pub struct BuildArgs {
    /// Explicit compiler file namespace and path; repeat as ENV=PATH.
    /// The selected platform must bind each request unchanged, without fallback.
    #[arg(long = "compiler-input", value_parser = CompilerFileSelection::parse)]
    pub compiler_inputs: Vec<CompilerFileSelection>,
    /// Explicit image configuration interpreted by the selected platform.
    #[arg(long)]
    pub image_config: Option<String>,
    /// Directory of the vendor firmware blobs the platform packs (e.g. AMD
    /// PSP firmware; see `fbuild blobs`).
    #[arg(long, conflicts_with = "donor")]
    pub blobs: Option<String>,
    /// Vendor flash image to take those blobs from, as `fbuild blobs`
    /// extracts them (into target/fstart-blobs/<board>/).
    #[arg(long)]
    pub donor: Option<String>,
    /// SMBIOS Type 0 release date in MM/DD/YYYY form.
    #[arg(long, value_parser = parse_smbios_date, default_value_t = current_smbios_date())]
    pub smbios_date: String,
    /// Physical address at which the bzImage protected-mode payload is loaded.
    #[arg(long, value_parser = parse_u64)]
    pub linux_kernel_load_addr: Option<u64>,
    /// Command line passed by the direct x86 Linux launcher.
    #[arg(long)]
    pub linux_bootargs: Option<String>,
    /// Dump BSP MTRRs and control registers immediately before Linux handoff.
    #[arg(long, default_value_t = false)]
    pub linux_print_mtrrs: bool,
    /// Log messages above this level are compiled out of every stage, and
    /// it becomes their default runtime level. Default: all compiled in,
    /// `info` at runtime.
    #[arg(long, value_enum, env = "FSTART_LOG_LEVEL")]
    pub log_level: Option<LogLevel>,
    /// File handed to a coreboot payload, as NAME=PATH; repeatable. The
    /// payload finds it by NAME, e.g. `vgaroms/seavgabios.bin` for SeaBIOS.
    #[arg(long = "payload-file", value_parser = parse_payload_file)]
    pub payload_files: Vec<(String, String)>,
    /// Ed25519 PKCS#8 private key (PEM or DER) that signs the image, e.g.
    /// from `openssl genpkey -algorithm ed25519`. Without it the image is
    /// signed with an ephemeral key that is discarded after the build.
    #[arg(long)]
    pub signing_key: Option<PathBuf>,
}

impl Default for BuildArgs {
    fn default() -> Self {
        Self {
            compiler_inputs: Vec::new(),
            image_config: None,
            blobs: None,
            donor: None,
            smbios_date: current_smbios_date(),
            linux_kernel_load_addr: None,
            linux_bootargs: None,
            linux_print_mtrrs: false,
            log_level: None,
            payload_files: Vec::new(),
            signing_key: None,
        }
    }
}

impl BuildArgs {
    pub fn selection(&self, payload: Option<PayloadChoice>) -> BuildSelection {
        BuildSelection {
            payload: payload.map(|choice| choice.as_str().to_owned()),
            compiler_files: self.compiler_inputs.clone(),
            image_config: self.image_config.clone(),
            blobs: self.blobs.clone(),
            donor: self.donor.clone(),
            manifests: Default::default(),
            smbios_release_date: Some(self.smbios_date.clone()),
            x86_linux_kernel_load_addr: self.linux_kernel_load_addr,
            x86_linux_bootargs: self.linux_bootargs.clone(),
            x86_linux_print_mtrrs: self.linux_print_mtrrs,
            log_level: self.log_level.map(|level| level.as_str().to_owned()),
        }
    }

    pub fn append_cli_args(&self, args: &mut Vec<String>) {
        for input in &self.compiler_inputs {
            args.extend([
                "--compiler-input".into(),
                format!("{}={}", input.environment, input.path),
            ]);
        }
        if let Some(path) = &self.image_config {
            args.extend(["--image-config".into(), path.clone()]);
        }
        if let Some(path) = &self.blobs {
            args.extend(["--blobs".into(), path.clone()]);
        }
        if let Some(path) = &self.donor {
            args.extend(["--donor".into(), path.clone()]);
        }
        args.extend(["--smbios-date".into(), self.smbios_date.clone()]);
        if let Some(value) = self.linux_kernel_load_addr {
            args.extend(["--linux-kernel-load-addr".into(), format!("{value:#x}")]);
        }
        if let Some(value) = &self.linux_bootargs {
            args.extend(["--linux-bootargs".into(), value.clone()]);
        }
        if self.linux_print_mtrrs {
            args.push("--linux-print-mtrrs".into());
        }
        if let Some(level) = self.log_level {
            args.extend(["--log-level".into(), level.as_str().into()]);
        }
        for (name, path) in &self.payload_files {
            args.extend(["--payload-file".into(), format!("{name}={path}")]);
        }
        if let Some(path) = &self.signing_key {
            args.extend(["--signing-key".into(), path.display().to_string()]);
        }
    }
}

fn current_smbios_date() -> String {
    let timestamp = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        });
    let (year, month, day) = civil_from_days((timestamp / 86_400) as i64);
    format!("{month:02}/{day:02}/{year:04}")
}

// Howard Hinnant's civil-from-days transform, with day zero at 1970-01-01.
fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
}

fn parse_smbios_date(value: &str) -> Result<String, String> {
    let mut fields = value.split('/');
    let month = fields
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or("SMBIOS date must use MM/DD/YYYY")?;
    let day = fields
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or("SMBIOS date must use MM/DD/YYYY")?;
    let year = fields
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or("SMBIOS date must use MM/DD/YYYY")?;
    if fields.next().is_some() || value.len() != 10 || !(1..=12).contains(&month) {
        return Err("SMBIOS date must use MM/DD/YYYY".into());
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day == 0 || day > days {
        return Err("SMBIOS date is not a valid calendar date".into());
    }
    Ok(value.to_owned())
}

fn parse_payload_file(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((name, path)) if !name.is_empty() && !path.is_empty() => {
            Ok((name.to_owned(), path.to_owned()))
        }
        _ => Err("payload file must be NAME=PATH".into()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_input_cli_round_trips_paths_and_keeps_duplicate_requests_visible() {
        #[derive(clap::Parser)]
        struct InputCli {
            #[command(flatten)]
            options: BuildArgs,
        }
        use clap::Parser;
        let cli = InputCli::try_parse_from([
            "inputs",
            "--compiler-input",
            "FSTART_BYTES=a path/file=1.bin",
            "--image-config",
            "a path/image=2.json",
            "--donor",
            "vendor.rom",
        ])
        .unwrap();
        let selected = cli.options.selection(None);
        selected.validate_compiler_files().unwrap();
        assert_eq!(selected.compiler_files[0].path, "a path/file=1.bin");
        assert_eq!(
            selected.image_config.as_deref(),
            Some("a path/image=2.json")
        );
        let mut forwarded = vec!["inputs".into()];
        cli.options.append_cli_args(&mut forwarded);
        let forwarded = InputCli::try_parse_from(forwarded)
            .unwrap()
            .options
            .selection(None);
        assert_eq!(
            forwarded.compiler_files[0].path,
            selected.compiler_files[0].path
        );
        assert_eq!(forwarded.image_config, selected.image_config);
        assert_eq!(forwarded.donor.as_deref(), Some("vendor.rom"));
        let duplicates = InputCli::try_parse_from([
            "inputs",
            "--compiler-input",
            "FILE=a",
            "--compiler-input",
            "FILE=b",
        ])
        .unwrap();
        assert!(
            duplicates
                .options
                .selection(None)
                .validate_compiler_files()
                .is_err()
        );
        assert!(
            InputCli::try_parse_from(["inputs", "--compiler-input", "RUSTFLAGS=file"]).is_err()
        );
    }

    #[test]
    fn civil_dates_cover_epoch_and_leap_day() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn smbios_date_parser_rejects_non_calendar_dates() {
        assert!(parse_smbios_date("02/29/2024").is_ok());
        assert!(parse_smbios_date("02/29/2025").is_err());
        assert!(parse_smbios_date("2025-02-28").is_err());
    }
}
