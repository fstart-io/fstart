//! End-to-end bare-metal QEMU validation for fstart MCTP + PLDM + SPDM (Host & BMC).
//!
//! Runs:
//! 1. Host `qemu-q35` SPDM 1.2 over PCIe DOE (NVMe `spdm_port` <-> `fstart-mctp-broker spdm-emu`)
//! 2. Dual-QEMU `ast2500-evb` BMC <-> `fstart-mctp-broker bridge` (Wireshark SLL2 `ETH_P_MCTP`
//!    `pcapng`) <-> `qemu-q35` Host over Serial (DSP0253)

use crate::pcapng::{ETH_P_MCTP, EpbHeader, IdbBlock, LINKTYPE_LINUX_SLL2, Sll2Header};
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use zerocopy::FromBytes;
use zerocopy::little_endian::U32 as LeU32;

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
            % 1_000_000;
        let name = format!("{prefix}-{}-{nanos}", std::process::id());
        // Unix domain sockets have a 108-byte SUN_LEN limit; fall back to /tmp
        // when TMPDIR is too long to host socket files safely.
        let base = std::env::temp_dir();
        let candidate = base.join(&name);
        let dir = if candidate.as_os_str().len() + 16 < 100 {
            candidate
        } else {
            PathBuf::from("/tmp").join(&name)
        };
        fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child present")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

struct LogState {
    lines: Vec<String>,
    eof: bool,
}

/// Continuously drains a child process's stdout in a background thread so pipes
/// never stall, while allowing callers to wait for one or more log markers.
struct LogWatcher {
    state: Arc<(Mutex<LogState>, Condvar)>,
}

impl LogWatcher {
    fn attach(child: &mut Child) -> io::Result<Self> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("child stdout not piped"))?;
        let state = Arc::new((
            Mutex::new(LogState {
                lines: Vec::new(),
                eof: false,
            }),
            Condvar::new(),
        ));
        let bg = Arc::clone(&state);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf)
                            .trim_end_matches(['\r', '\n'])
                            .to_owned();
                        let (lock, cv) = &*bg;
                        let mut guard = lock.lock().unwrap();
                        guard.lines.push(line);
                        cv.notify_all();
                    }
                }
            }
            let (lock, cv) = &*bg;
            let mut guard = lock.lock().unwrap();
            guard.eof = true;
            cv.notify_all();
        });
        Ok(Self { state })
    }

    fn wait_for(&self, marker: &str, timeout: Duration, label: &str) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        let (lock, cv) = &*self.state;
        let mut guard = lock.lock().unwrap();
        loop {
            if guard.lines.iter().any(|line| line.contains(marker)) {
                return Ok(());
            }
            if guard.eof {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (g, _) = cv.wait_timeout(guard, deadline - now).unwrap();
            guard = g;
        }
        eprintln!("[{label}] TIMEOUT/EOF waiting for {marker:?}. Last 20 lines:");
        let start = guard.lines.len().saturating_sub(20);
        for line in &guard.lines[start..] {
            eprintln!("  {line}");
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("[{label}] did not emit marker {marker:?}"),
        ))
    }
}

fn workspace_root() -> io::Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists()
            && fs::read_to_string(&cargo_toml)
                .map(|s| s.contains("[workspace]"))
                .unwrap_or(false)
        {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "could not find fstart workspace root",
            ));
        }
    }
}

fn find_latest_image(root: &Path, board: &str, profile: &str, suffix: &str) -> io::Result<PathBuf> {
    let base = root.join("target/fstart-build").join(board).join(profile);
    let mut best: Option<(SystemTime, PathBuf)> = None;
    if let Ok(hashes) = fs::read_dir(&base) {
        for hash_entry in hashes.flatten() {
            let img_root = hash_entry.path().join("image");
            if let Ok(variants) = fs::read_dir(&img_root) {
                for var_entry in variants.flatten() {
                    if let Ok(files) = fs::read_dir(var_entry.path()) {
                        for file_entry in files.flatten() {
                            let path = file_entry.path();
                            let name_ok = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.ends_with(suffix));
                            if name_ok
                                && let Ok(meta) = file_entry.metadata()
                                && let Ok(mtime) = meta.modified()
                                && best.as_ref().is_none_or(|(t, _)| mtime > *t)
                            {
                                best = Some((mtime, path));
                            }
                        }
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p).ok_or_else(|| {
        let rel = if profile == "release" {
            " --release"
        } else {
            ""
        };
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no built image matching {board}/{profile}/*{suffix}; run \
                 `cargo run -p fbuild -- build --board {board} --debug mctp-validate{rel}` first"
            ),
        )
    })
}

/// Parse pcapng SHB, IDB (`LINKTYPE_LINUX_SLL2 = 276`), and Enhanced Packet Blocks
/// carrying `ETH_P_MCTP (0x00FA)`. Returns the verified MCTP packet count.
pub fn verify_pcapng(pcap_path: &Path) -> io::Result<usize> {
    let data = fs::read(pcap_path)?;
    let mut off = 0usize;
    let mut saw_shb = false;
    let mut saw_idb = false;
    let mut mctp_packets = 0usize;

    while off + 12 <= data.len() {
        let block_slice = &data[off..];
        let (btype, _) = LeU32::read_from_prefix(block_slice).unwrap();
        let (blen_raw, _) = LeU32::read_from_prefix(&block_slice[4..]).unwrap();
        let blen = blen_raw.get() as usize;
        if blen < 12 || (blen & 3) != 0 || off + blen > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed pcapng block at {off} (len={blen})"),
            ));
        }
        let block = &block_slice[..blen];
        let (trailer, _) = LeU32::read_from_prefix(&block[blen - 4..]).unwrap();
        if trailer.get() as usize != blen {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "pcapng trailing length mismatch at {off}: {} != {blen}",
                    trailer.get()
                ),
            ));
        }
        match btype.get() {
            0x0A0D_0D0A => saw_shb = true,
            0x0000_0001 => {
                let (idb, _) = IdbBlock::ref_from_prefix(block)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "truncated IDB"))?;
                if idb.link_type.get() != LINKTYPE_LINUX_SLL2 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "expected LINKTYPE_LINUX_SLL2 ({LINKTYPE_LINUX_SLL2}), got {}",
                            idb.link_type.get()
                        ),
                    ));
                }
                saw_idb = true;
            }
            0x0000_0006 => {
                let (epb, rest) = EpbHeader::ref_from_prefix(block)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "truncated EPB"))?;
                let cap_len = epb.cap_len.get() as usize;
                if rest.len() < cap_len + 4 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("EPB cap_len {cap_len} exceeds block length {blen}"),
                    ));
                }
                let pkt = &rest[..cap_len];
                let (sll2, mctp) = Sll2Header::ref_from_prefix(pkt).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("short SLL2+MCTP packet ({}B)", pkt.len()),
                    )
                })?;
                if mctp.len() < 4 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("short MCTP packet ({}B)", mctp.len()),
                    ));
                }
                if sll2.protocol_type.get() != ETH_P_MCTP {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "expected ETH_P_MCTP ({ETH_P_MCTP:#x}), got {:#x}",
                            sll2.protocol_type.get()
                        ),
                    ));
                }
                if (mctp[0] & 0x0F) != 0x01 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("bad MCTP version byte {:#x}", mctp[0]),
                    ));
                }
                mctp_packets += 1;
            }
            _ => {}
        }
        off += blen;
    }

    if !(saw_shb && saw_idb && mctp_packets >= 10) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pcapng incomplete: shb={saw_shb}, idb={saw_idb}, mctp_packets={mctp_packets}"),
        ));
    }
    Ok(mctp_packets)
}

fn test_pcie_doe(broker_exe: &Path, q35_bios: &Path) -> io::Result<()> {
    eprintln!("=== Test 1: qemu-q35 Host SPDM 1.2 over PCIe DOE ===");
    let tmp = TempDir::new("fstart-doe")?;
    let nvme_img = tmp.path().join("nvme.img");
    fs::write(&nvme_img, vec![0u8; 1024 * 1024])?;
    let port = 23231u16;

    let _broker = ChildGuard::new(
        Command::new(broker_exe)
            .args(["spdm-emu", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    thread::sleep(Duration::from_millis(300));

    let mut qemu = ChildGuard::new(
        Command::new("qemu-system-x86_64")
            .args([
                "-machine",
                "q35",
                "-accel",
                "tcg",
                "-cpu",
                "max",
                "-m",
                "1G",
                "-no-reboot",
                "-vga",
                "none",
                "-nographic",
                "-bios",
            ])
            .arg(q35_bios)
            .args([
                "-drive",
                &format!("file={},format=raw,if=none,id=drv0", nvme_img.display()),
                "-device",
                &format!("nvme,serial=fstart0,drive=drv0,spdm_port={port},spdm_trans=doe"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let log = LogWatcher::attach(qemu.child_mut())?;
    log.wait_for(
        "qemu-q35: SPDM-DOE PASS",
        Duration::from_secs(30),
        "q35-doe",
    )?;
    eprintln!("PASS: qemu-q35 SPDM 1.2 over PCIe DOE (NVMe)");
    Ok(())
}

fn test_dual_qemu_serial(
    broker_exe: &Path,
    q35_bios: &Path,
    bmc_flash: &Path,
    pcap_path: &Path,
) -> io::Result<()> {
    eprintln!("=== Test 2: Dual-QEMU ast2500-evb BMC <-> broker <-> qemu-q35 Host (Serial) ===");
    let tmp = TempDir::new("fstart-dual")?;
    let bmc_sock = tmp.path().join("bmc.sock");
    let host_sock = tmp.path().join("host.sock");
    let padded_bmc = tmp.path().join("bmc-64m.bin");

    let mut raw = fs::read(bmc_flash)?;
    const TARGET_SZ: usize = 64 * 1024 * 1024;
    if raw.len() < TARGET_SZ {
        raw.resize(TARGET_SZ, 0xFF);
    } else {
        raw.truncate(TARGET_SZ);
    }
    fs::write(&padded_bmc, &raw)?;

    let _broker = ChildGuard::new(
        Command::new(broker_exe)
            .arg("bridge")
            .arg("--bmc-sock")
            .arg(&bmc_sock)
            .arg("--host-sock")
            .arg(&host_sock)
            .args(["--transport", "serial"])
            .arg("--pcap")
            .arg(pcap_path)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    for _ in 0..50 {
        if bmc_sock.exists() && host_sock.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    let mut bmc = ChildGuard::new(
        Command::new("qemu-system-arm")
            .args([
                "-M",
                "ast2500-evb,execute-in-place=on,fmc-model=mt25ql512ab",
                "-no-reboot",
                "-display",
                "none",
                "-monitor",
                "none",
                "-drive",
                &format!("file={},format=raw,if=mtd", padded_bmc.display()),
                "-chardev",
                &format!("socket,id=mctp,path={}", bmc_sock.display()),
                "-serial",
                "stdio",
                "-serial",
                "chardev:mctp",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let bmc_log = LogWatcher::attach(bmc.child_mut())?;
    bmc_log.wait_for(
        "ast2500 mainstage: entered",
        Duration::from_secs(90),
        "ast2500-boot",
    )?;

    let mut host = ChildGuard::new(
        Command::new("qemu-system-x86_64")
            .args([
                "-machine",
                "q35",
                "-accel",
                "tcg",
                "-cpu",
                "max",
                "-m",
                "1G",
                "-no-reboot",
                "-vga",
                "none",
                "-display",
                "none",
                "-monitor",
                "none",
                "-bios",
            ])
            .arg(q35_bios)
            .args([
                "-chardev",
                &format!("socket,id=mctp,path={}", host_sock.display()),
                "-serial",
                "stdio",
                "-serial",
                "chardev:mctp",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let host_log = LogWatcher::attach(host.child_mut())?;
    host_log.wait_for(
        "qemu-q35: MCTP+PLDM+SPDM-SERIAL PASS",
        Duration::from_secs(60),
        "q35-serial",
    )?;
    bmc_log.wait_for(
        "ast2500-bmc: MCTP+PLDM+SPDM-SERIAL PASS",
        Duration::from_secs(15),
        "ast2500-serial",
    )?;

    let count = verify_pcapng(pcap_path)?;
    eprintln!(
        "PASS: Dual-QEMU Serial BMC <-> Host (pcapng: {}, {count} verified SLL2 ETH_P_MCTP packets)",
        pcap_path.display()
    );
    Ok(())
}

pub fn run(q35_bios: Option<PathBuf>, bmc_flash: Option<PathBuf>, pcap: PathBuf) -> io::Result<()> {
    let broker_exe = std::env::current_exe()?;
    let root = workspace_root()?;
    let q35_bios = match q35_bios {
        Some(p) => p,
        None => find_latest_image(&root, "qemu-q35", "debug", "qemu-q35.rom")?,
    };
    let bmc_flash = match bmc_flash {
        Some(p) => p,
        None => find_latest_image(&root, "gigabyte-mj11-ec1-bmc", "release", "-64m.pflash")?,
    };
    eprintln!("Using q35 BIOS:  {}", q35_bios.display());
    eprintln!("Using BMC flash: {}", bmc_flash.display());
    test_pcie_doe(&broker_exe, &q35_bios)?;
    test_dual_qemu_serial(&broker_exe, &q35_bios, &bmc_flash, &pcap)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcapng::PcapNgWriter;

    #[test]
    fn verify_pcapng_accepts_valid_sll2_and_rejects_corrupt_trailer() {
        let tmp = TempDir::new("fstart-pcap-test").unwrap();
        let path = tmp.path().join("test.pcapng");
        {
            let mut w = PcapNgWriter::create(&path).unwrap();
            for i in 0u8..12 {
                // Valid 4-byte MCTP header (ver=1) + 4-byte payload
                w.write_mctp_packet(&[0x01, 0x0A, 0x08, 0xC8, 0x00, 0x80, i, 0x00])
                    .unwrap();
            }
        }
        assert_eq!(verify_pcapng(&path).unwrap(), 12);

        // Corrupt the final block's trailing length
        let mut bytes = fs::read(&path).unwrap();
        let n = bytes.len();
        bytes[n - 1] ^= 0xFF;
        fs::write(&path, &bytes).unwrap();
        assert!(verify_pcapng(&path).is_err());
    }
}
