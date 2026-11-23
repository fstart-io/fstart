//! A fresh software TPM for one QEMU run (`fbuild qemu --tpm`, the
//! measured-boot tests): swtpm, set up with the requested family and PCR
//! banks, serving the control socket QEMU's TPM device connects to.

use fstart_image_build::boot_test::{Bank, Tpm, TpmFamily};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// A running swtpm; stopped, and its state removed, on drop.
pub struct Swtpm {
    state: PathBuf,
    child: Child,
}

impl Swtpm {
    /// Start a TPM with fresh state. The state lives in a directory of its
    /// own under the system's temporary directory: QEMU and swtpm talk over
    /// a Unix socket in it, whose path must stay short.
    pub fn start(tpm: &Tpm) -> io::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let temp = std::env::temp_dir();
        let base = if temp.as_os_str().len() > 60 {
            PathBuf::from("/tmp")
        } else {
            temp
        };
        let state = base.join(format!(
            "fs-tpm-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&state);
        fs::create_dir_all(&state)?;
        let tpm2 = tpm.family == TpmFamily::Tpm20;
        let mut setup = Command::new("swtpm_setup");
        setup.args(["--tpmstate".as_ref(), state.as_os_str()]);
        if tpm2 {
            let banks: Vec<_> = tpm.banks.iter().map(|bank| bank.name()).collect();
            setup.args(["--tpm2", "--pcr-banks", &banks.join(",")]);
        }
        let setup = setup
            .args(["--createek", "--overwrite"])
            .stdout(fs::File::create(state.join("setup.log"))?)
            .stderr(Stdio::null())
            .status()?;
        if !setup.success() {
            let _ = fs::remove_dir_all(&state);
            return Err(io::Error::other(format!("swtpm_setup {setup}")));
        }
        let arg = |prefix: &str, path: PathBuf| {
            let mut arg = OsString::from(prefix);
            arg.push(path);
            arg
        };
        let mut swtpm = Command::new("swtpm");
        // Gone with the QEMU it served, even if fbuild is killed.
        swtpm.args(["socket", "--terminate"]);
        if tpm2 {
            swtpm.arg("--tpm2");
        }
        let child = swtpm
            .arg("--tpmstate")
            .arg(arg("dir=", state.clone()))
            .arg("--ctrl")
            .arg(arg("type=unixio,path=", state.join("sock")))
            .arg("--log")
            .arg(arg("level=1,file=", state.join("swtpm.log")))
            .stdin(Stdio::null())
            .spawn()?;
        let swtpm = Self { state, child };
        let start = Instant::now();
        while !swtpm.socket().exists() {
            if start.elapsed() > Duration::from_secs(10) {
                return Err(io::Error::other("swtpm opened no control socket"));
            }
            thread::sleep(Duration::from_millis(50));
        }
        Ok(swtpm)
    }

    /// The control socket, for [`crate::qemu::Launch::tpm`].
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.state.join("sock")
    }

    /// Keep the TPM's logs next to a boot's `log`.
    pub fn keep_logs(&self, log: &Path) {
        for (name, kept) in [("setup.log", "swtpm-setup.log"), ("swtpm.log", "swtpm.log")] {
            let _ = fs::copy(self.state.join(name), log.with_extension(kept));
        }
    }
}

impl Drop for Swtpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.state);
    }
}

/// `--tpm 1.2|2.0[:BANK,...]`: a TPM 1.2, or a TPM 2.0 with these PCR banks
/// (SHA-1 and SHA-256 by default).
pub fn parse(value: &str) -> Result<Tpm, String> {
    let (family, banks) = value.split_once(':').unwrap_or((value, ""));
    let family = match family {
        "1.2" => TpmFamily::Tpm12,
        "2.0" => TpmFamily::Tpm20,
        other => return Err(format!("TPM family {other}: expected 1.2 or 2.0")),
    };
    let banks = if banks.is_empty() {
        match family {
            TpmFamily::Tpm12 => vec![Bank::Sha1],
            TpmFamily::Tpm20 => vec![Bank::Sha1, Bank::Sha256],
        }
    } else if family == TpmFamily::Tpm12 {
        return Err("a TPM 1.2 has only its SHA-1 bank".into());
    } else {
        banks
            .split(',')
            .map(|name| {
                Bank::ALL
                    .into_iter()
                    .find(|bank| bank.name() == name)
                    .ok_or_else(|| format!("unknown PCR bank {name}"))
            })
            .collect::<Result<_, _>>()?
    };
    Ok(Tpm { family, banks })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tpm_arguments_name_a_family_and_its_banks() {
        assert_eq!(parse("1.2").unwrap().banks, [Bank::Sha1]);
        assert_eq!(parse("2.0").unwrap().banks, [Bank::Sha1, Bank::Sha256]);
        let tpm = parse("2.0:sha256,sha384").unwrap();
        assert_eq!(tpm.family, TpmFamily::Tpm20);
        assert_eq!(tpm.banks, [Bank::Sha256, Bank::Sha384]);
        for bad in ["1.3", "1.2:sha256", "2.0:md5"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
