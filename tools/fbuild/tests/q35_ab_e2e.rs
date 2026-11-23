// SPDX-License-Identifier: MIT OR Apache-2.0
//! End-to-end QEMU Q35 RO/A/B + UEFI Capsule-on-Disk + TPM 2.0 rollback floor tests.
//!
//! Exercises the real two-stage `qemu-q35-ab` board under KVM + dual
//! `pflash_cfi01` (`unit=0,readonly=on` RO + `unit=1` RW) + `swtpm` TPM 2.0 +
//! AHCI GPT/FAT16 ESP:
//!
//! 1. Happy path + TPM rollback floor downgrade rejection + tampered capsule rejection:
//!    - Factory boot v1 (`fw=1, sec=1`, Slot A), auto-provisions TPM floor = 1,
//!      write-locks TPM NV, applies signed v2 (`fw=2, sec=2`) capsule from
//!      `\EFI\UpdateCapsule\UPDATE.CAP`, cold resets.
//!    - Trial boot v2 (Slot B), confirms trial, copies Slot B -> Slot A
//!      (byte-for-byte verified sync-before-bump), persists `Phase::FloorPending`.
//!    - Next cold boot: RO verifies both slots, advances TPM NV floor 1 -> 2,
//!      finalizes `SLOT_STATE` to `Phase::Stable`, locks RW flash on normal boot.
//!    - Downgrade attempt: validly-signed v3 (`fw=3, sec=1`) passes ESRT LSV=1
//!      and PKCS#7 verification but is rejected by `FstartCapsuleBackend` because
//!      `sec (1) < TPM floor (2)`.
//!    - Tampered capsule attempt: corrupted PKCS#7 signature is rejected without
//!      modifying `SLOT_STATE` or either RW slot.
//!    - Verifies `ro.pflash` is byte-for-byte unmodified across the entire suite.
//!
//! 2. Broken trial exhaustion + automatic fallback:
//!    - Applies signed broken v2 (`fw=2, sec=2, fail-trial=true`) capsule.
//!    - Across 3 automatic cold resets RO debits trial attempts 3 -> 2 -> 1 -> 0.
//!    - On boot 5 RO falls back to Slot A (`Phase::Stable`, `last_attempt_ver=2`,
//!      `last_attempt_status=1`) and boots cleanly without bumping the TPM floor.
//!    - Subsequent cold boot stays cleanly on Slot A (`Phase::Stable`, floor = 1).
//!
//! 3. Fail-closed without TPM:
//!    - Boots without `swtpm` and verifies RO halts immediately without launching
//!      any RW stage.
//!
//! Run with:
//! ```bash
//! nix develop --command bash -c \
//!   'unset LD_LIBRARY_PATH; cargo test -p fbuild --test q35_ab_e2e -- --ignored --nocapture --test-threads=1'
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use fbuild::board_manifest::{self, BoardManifest};
use fbuild::build_board::workspace_root;
use fbuild::plan_executor::{AssembleRequest, Resolved};
use fstart_ffs::slot_state::{Phase, SECTOR_SIZE, ScanStatus, Slot, scan_journal};
use fstart_image_build::capsule::{CapsuleSpec, seed_os_indications_varstore, sign_fmp_capsule};
use fstart_image_build::options::Settings;
use fstart_image_build::pkcs7_sign::dev_capsule_key_and_cert;
use fstart_image_build::plan::BuildSelection;
use fstart_platform_qemu::q35_ab::{
    Q35_AB_FIRMWARE_GUID, Q35_AB_FLASH_SIZE, Q35_AB_NVRAM_OFFSET, Q35_AB_NVRAM_SIZE,
    Q35_AB_RO_SIZE, Q35_AB_RW_A_SIZE, Q35_AB_RW_SIZE, Q35_AB_STATE_SECTOR_0, Q35_AB_STATE_SECTOR_1,
};

/// PKCS#8 v1 DER prefix for an Ed25519 32-byte private key seed (RFC 8410).
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

fn write_ed25519_pkcs8_der(path: &Path, seed: [u8; 32]) {
    let mut der = Vec::with_capacity(48);
    der.extend_from_slice(&ED25519_PKCS8_PREFIX);
    der.extend_from_slice(&seed);
    fs::write(path, der).expect("write Ed25519 PKCS#8 DER");
}

struct BuiltAbArtifacts {
    rom: PathBuf,
    rw_ffs: PathBuf,
}

fn build_q35_ab(
    root: &Path,
    manifest: &BoardManifest,
    signing_key: &Path,
    fw_version: u32,
    security_version: u64,
    fail_trial: bool,
) -> BuiltAbArtifacts {
    let mut options = BTreeMap::new();
    options.insert("fw-version".to_string(), fw_version.to_string());
    options.insert("security-version".to_string(), security_version.to_string());
    options.insert("dev-keys".to_string(), "true".to_string());
    if fail_trial {
        options.insert("fail-trial".to_string(), "true".to_string());
    }
    let selection = BuildSelection {
        payload: Some("uefi".to_string()),
        options,
        ..Default::default()
    };
    let resolved =
        Resolved::load(root, manifest, selection).expect("resolve qemu-q35-ab build plan");
    let supplied = BTreeMap::new();
    let settings = Settings::default();
    let assembled = resolved
        .assemble(
            root,
            manifest,
            &AssembleRequest {
                release: true,
                supplied: &supplied,
                payload_files: &[],
                signing_key: Some(signing_key),
                settings: &settings,
            },
        )
        .expect("assemble qemu-q35-ab image");
    let rw_ffs = assembled
        .artifacts
        .get("ffs")
        .cloned()
        .expect("ffs artifact in qemu-q35-ab build");
    BuiltAbArtifacts {
        rom: assembled.image,
        rw_ffs,
    }
}

/// Dual-pflash images split from a full 8 MiB `qemu-q35-ab` ROM:
/// - `ro`: upper `Q35_AB_RO_SIZE` (1.5 MiB), mounted `unit=0,readonly=on`
/// - `rw`: lower `Q35_AB_RW_SIZE` (6.5 MiB), mounted `unit=1` (writable)
struct DualPflash {
    ro: PathBuf,
    rw: PathBuf,
    ro_original: Vec<u8>,
}

impl DualPflash {
    fn from_full_rom(dir: &Path, rom_path: &Path) -> Self {
        let rom = fs::read(rom_path).expect("read full 8 MiB ROM");
        assert_eq!(rom.len(), Q35_AB_FLASH_SIZE as usize);
        let rw_len = Q35_AB_RW_SIZE as usize;
        let ro_len = Q35_AB_RO_SIZE as usize;
        assert_eq!(rw_len + ro_len, rom.len());

        let rw = dir.join("rw.pflash");
        let ro = dir.join("ro.pflash");
        let ro_original = rom[rw_len..].to_vec();
        fs::write(&rw, &rom[..rw_len]).expect("write rw.pflash");
        fs::write(&ro, &ro_original).expect("write ro.pflash");
        Self {
            ro,
            rw,
            ro_original,
        }
    }

    /// Write a fresh EDK2 authenticated variable store containing `OsIndications = 0x04`
    /// into the 256 KiB NVRAM window of `rw.pflash`.
    fn seed_nvram_os_indications(&self) {
        let mut rw = fs::read(&self.rw).expect("read rw.pflash");
        assert_eq!(rw.len(), Q35_AB_RW_SIZE as usize);
        let nvram = seed_os_indications_varstore(Q35_AB_NVRAM_SIZE as usize)
            .expect("seed OsIndications NVRAM");
        let start = Q35_AB_NVRAM_OFFSET as usize;
        rw[start..start + nvram.len()].copy_from_slice(&nvram);
        fs::write(&self.rw, rw).expect("write rw.pflash with OsIndications");
    }

    fn inspect_slot_state(&self) -> ScanStatus {
        let rw = fs::read(&self.rw).expect("read rw.pflash");
        let s0 = Q35_AB_STATE_SECTOR_0 as usize;
        let s1 = Q35_AB_STATE_SECTOR_1 as usize;
        scan_journal(&rw[s0..s0 + SECTOR_SIZE], &rw[s1..s1 + SECTOR_SIZE]).status
    }

    fn assert_ro_unmodified(&self) {
        let current = fs::read(&self.ro).expect("read ro.pflash");
        assert_eq!(
            current, self.ro_original,
            "RO pflash image was modified despite readonly=on!"
        );
    }
}

/// Persistent `swtpm` TPM 2.0 state directory shared across multiple QEMU boots.
/// Placed under `/tmp` (when available) via `tempfile::Builder` so the Unix
/// control socket path stays well under Linux's 108-byte `sun_path` limit and
/// never collides across parallel runs.
struct PersistentTpm {
    dir: tempfile::TempDir,
}

impl PersistentTpm {
    fn create(label: &str) -> Self {
        let base = if Path::new("/tmp").is_dir() {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let dir = tempfile::Builder::new()
            .prefix(&format!("fs-ab-{label}-"))
            .tempdir_in(&base)
            .expect("create swtpm tempdir");
        let out = Command::new("swtpm_setup")
            .args(["--tpmstate"])
            .arg(dir.path())
            .args([
                "--tpm2",
                "--pcr-banks",
                "sha1,sha256",
                "--createek",
                "--overwrite",
            ])
            .output()
            .expect("run swtpm_setup");
        assert!(
            out.status.success(),
            "swtpm_setup failed ({}):\nstdout: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        Self { dir }
    }

    fn spawn(&self) -> RunningSwtpm {
        let sock = self.dir.path().join("sock");
        let log_path = self.dir.path().join("swtpm.log");
        let _ = fs::remove_file(&sock);
        let child = Command::new("swtpm")
            .args(["socket", "--terminate", "--tpm2", "--tpmstate"])
            .arg(format!("dir={}", self.dir.path().display()))
            .arg("--ctrl")
            .arg(format!("type=unixio,path={}", sock.display()))
            .arg("--log")
            .arg(format!("level=1,file={}", log_path.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn swtpm");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !sock.exists() {
            assert!(
                Instant::now() < deadline,
                "swtpm did not create socket at {} (log: {})",
                sock.display(),
                fs::read_to_string(&log_path).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(25));
        }
        RunningSwtpm { child, sock }
    }
}

struct RunningSwtpm {
    child: Child,
    sock: PathBuf,
}

impl RunningSwtpm {
    fn finish(mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for RunningSwtpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run QEMU Q35 against dual `pflash` + optional persistent `tpm` (+ optional
/// AHCI ESP disk), streaming serial output until `stop_marker` appears
/// `stop_count` times or `timeout` expires. Returns the full captured serial log.
fn run_qemu_until(
    flash: &DualPflash,
    tpm: Option<&PersistentTpm>,
    esp_disk: Option<&Path>,
    allow_reboot: bool,
    stop_marker: &str,
    stop_count: usize,
    timeout: Duration,
) -> String {
    let swtpm = tpm.map(PersistentTpm::spawn);
    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args([
        "-machine",
        "q35",
        "-accel",
        "kvm",
        "-cpu",
        "host",
        "-m",
        "1G",
        "-smp",
        "1",
        "-global",
        "ICH9-LPC.disable_s3=1",
    ])
    .args([
        "-drive",
        &format!(
            "if=pflash,file={},format=raw,unit=0,readonly=on",
            flash.ro.display()
        ),
        "-drive",
        &format!("if=pflash,file={},format=raw,unit=1", flash.rw.display()),
    ])
    .args([
        "-device",
        "bochs-display",
        "-display",
        "none",
        "-vga",
        "none",
    ])
    .args([
        "-chardev",
        "stdio,id=char0,mux=off,signal=off",
        "-serial",
        "chardev:char0",
    ]);
    if let Some(ref running) = swtpm {
        cmd.args([
            "-chardev",
            &format!("socket,id=chrtpm,path={}", running.sock.display()),
            "-tpmdev",
            "emulator,id=tpm0,chardev=chrtpm",
            "-device",
            "tpm-tis,tpmdev=tpm0",
        ]);
    }
    if !allow_reboot {
        cmd.arg("-no-reboot");
    }
    if let Some(disk) = esp_disk {
        cmd.args([
            "-drive",
            &format!("id=esp,if=none,format=raw,file={}", disk.display()),
            "-device",
            "ide-hd,drive=esp,bus=ide.0",
        ]);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn qemu-system-x86_64");
    let stdout = child.stdout.take().expect("qemu stdout");
    let mut stderr = child.stderr.take().expect("qemu stderr");
    let (tx, rx) = mpsc::channel::<String>();
    let reader = thread::spawn(move || {
        let mut br = BufReader::new(stdout);
        let mut line = String::new();
        while br.read_line(&mut line).unwrap_or(0) > 0 {
            let _ = tx.send(std::mem::take(&mut line));
        }
    });
    let stderr_reader = thread::spawn(move || {
        let mut err_text = String::new();
        let _ = stderr.read_to_string(&mut err_text);
        err_text
    });

    let mut log = String::new();
    let mut seen = 0usize;
    let deadline = Instant::now() + timeout;
    let mut early_exit = None;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(line) => {
                log.push_str(&line);
                if line.contains(stop_marker) {
                    seen += 1;
                    if seen >= stop_count {
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(status)) = child.try_wait() {
                    early_exit = Some(status);
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.kill();
    let exit_status = early_exit.or_else(|| child.wait().ok());
    let _ = reader.join();
    let stderr_text = stderr_reader.join().unwrap_or_default();
    // Drain any final lines buffered before EOF.
    while let Ok(line) = rx.try_recv() {
        log.push_str(&line);
    }
    if let Some(running) = swtpm {
        running.finish();
    }
    assert!(
        seen >= stop_count,
        "QEMU did not reach {stop_count}x `{stop_marker}` (saw {seen}, exit={exit_status:?}).\n--- qemu stderr ---\n{stderr_text}\n--- serial log ---\n{log}"
    );
    log
}

// ---------------------------------------------------------------------------
// Minimal pure-Rust GPT + FAT16 EFI System Partition builder
// ---------------------------------------------------------------------------

fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn lfn_checksum(sfn: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &b in sfn {
        sum = sum.rotate_right(1).wrapping_add(b);
    }
    sum
}

/// Build an 8 MiB raw GPT disk containing a FAT16 EFI System Partition with
/// `\EFI\UpdateCapsule\UPDATE.CAP` populated from `capsule`.
fn build_esp_disk_with_capsule(capsule: &[u8]) -> Vec<u8> {
    const SECTOR: usize = 512;
    const TOTAL_SECTORS: usize = 16_384; // 8 MiB
    const PART_FIRST_LBA: usize = 2048;
    const PART_LAST_LBA: usize = TOTAL_SECTORS - 34; // 16350
    const PART_SECTORS: usize = PART_LAST_LBA - PART_FIRST_LBA + 1; // 14303

    // FAT16 parameters: 2 sectors/cluster (1024 B) -> ~7107 clusters in [4085, 65524].
    const SECTORS_PER_CLUSTER: usize = 2;
    const CLUSTER_BYTES: usize = SECTOR * SECTORS_PER_CLUSTER;
    const RESERVED_SECTORS: usize = 1;
    const NUM_FATS: usize = 2;
    const ROOT_ENTRY_COUNT: usize = 512;
    const ROOT_DIR_SECTORS: usize = (ROOT_ENTRY_COUNT * 32) / SECTOR; // 32
    const SECTORS_PER_FAT: usize = 28; // 28 * 256 = 7168 FAT16 entries

    let file_clusters = capsule.len().div_ceil(CLUSTER_BYTES).max(1);
    let data_sectors =
        PART_SECTORS - RESERVED_SECTORS - NUM_FATS * SECTORS_PER_FAT - ROOT_DIR_SECTORS;
    let total_clusters = data_sectors / SECTORS_PER_CLUSTER;
    assert!(
        (4085..=65_524).contains(&total_clusters),
        "FAT16 cluster count out of range: {total_clusters}"
    );
    assert!(
        4 + file_clusters <= total_clusters + 2,
        "capsule ({} bytes) does not fit in ESP",
        capsule.len()
    );

    let mut img = vec![0u8; TOTAL_SECTORS * SECTOR];

    // 1. Protective MBR (LBA 0)
    let mbr = &mut img[0..SECTOR];
    let p = &mut mbr[446..462];
    p[4] = 0xEE; // GPT Protective
    p[8..12].copy_from_slice(&1u32.to_le_bytes());
    p[12..16].copy_from_slice(&((TOTAL_SECTORS - 1) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    // 2. GPT partition entries (128 * 128 B = 32 sectors)
    let mut entries = vec![0u8; 128 * 128];
    // EFI System Partition Type GUID: C12A7328-F81F-11D2-BA4B-00A0C93EC93B
    let esp_type_guid: [u8; 16] = [
        0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9,
        0x3b,
    ];
    let esp_part_guid: [u8; 16] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc,
        0xfe,
    ];
    entries[0..16].copy_from_slice(&esp_type_guid);
    entries[16..32].copy_from_slice(&esp_part_guid);
    entries[32..40].copy_from_slice(&(PART_FIRST_LBA as u64).to_le_bytes());
    entries[40..48].copy_from_slice(&(PART_LAST_LBA as u64).to_le_bytes());
    for (i, ch) in "ESP".encode_utf16().enumerate() {
        entries[56 + i * 2..56 + i * 2 + 2].copy_from_slice(&ch.to_le_bytes());
    }
    let entries_crc = crc32_ieee(&entries);

    let write_gpt_header = |buf: &mut [u8], my_lba: u64, alt_lba: u64, entries_lba: u64| {
        buf[0..8].copy_from_slice(b"EFI PART");
        buf[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        buf[12..16].copy_from_slice(&92u32.to_le_bytes());
        buf[24..32].copy_from_slice(&my_lba.to_le_bytes());
        buf[32..40].copy_from_slice(&alt_lba.to_le_bytes());
        buf[40..48].copy_from_slice(&34u64.to_le_bytes());
        buf[48..56].copy_from_slice(&(PART_LAST_LBA as u64).to_le_bytes());
        buf[56..72].copy_from_slice(&[0x42; 16]); // disk GUID
        buf[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        buf[80..84].copy_from_slice(&128u32.to_le_bytes());
        buf[84..88].copy_from_slice(&128u32.to_le_bytes());
        buf[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let hdr_crc = crc32_ieee(&buf[0..92]);
        buf[16..20].copy_from_slice(&hdr_crc.to_le_bytes());
    };

    // Primary GPT (LBA 1 + LBA 2..33)
    write_gpt_header(
        &mut img[SECTOR..2 * SECTOR],
        1,
        (TOTAL_SECTORS - 1) as u64,
        2,
    );
    img[2 * SECTOR..2 * SECTOR + entries.len()].copy_from_slice(&entries);
    // Backup GPT (LBA 16351..16382 + LBA 16383)
    let backup_entries_lba = TOTAL_SECTORS - 33;
    img[backup_entries_lba * SECTOR..backup_entries_lba * SECTOR + entries.len()]
        .copy_from_slice(&entries);
    write_gpt_header(
        &mut img[(TOTAL_SECTORS - 1) * SECTOR..TOTAL_SECTORS * SECTOR],
        (TOTAL_SECTORS - 1) as u64,
        1,
        backup_entries_lba as u64,
    );

    // 3. FAT16 BPB at PART_FIRST_LBA
    let vol_off = PART_FIRST_LBA * SECTOR;
    {
        let bpb = &mut img[vol_off..vol_off + SECTOR];
        bpb[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
        bpb[3..11].copy_from_slice(b"FSTART  ");
        bpb[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        bpb[13] = SECTORS_PER_CLUSTER as u8;
        bpb[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
        bpb[16] = NUM_FATS as u8;
        bpb[17..19].copy_from_slice(&(ROOT_ENTRY_COUNT as u16).to_le_bytes());
        bpb[19..21].copy_from_slice(&(PART_SECTORS as u16).to_le_bytes());
        bpb[21] = 0xF8;
        bpb[22..24].copy_from_slice(&(SECTORS_PER_FAT as u16).to_le_bytes());
        bpb[24..26].copy_from_slice(&32u16.to_le_bytes());
        bpb[26..28].copy_from_slice(&64u16.to_le_bytes());
        bpb[28..32].copy_from_slice(&(PART_FIRST_LBA as u32).to_le_bytes());
        bpb[36] = 0x80;
        bpb[38] = 0x29;
        bpb[39..43].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        bpb[43..54].copy_from_slice(b"FSTART ESP ");
        bpb[54..62].copy_from_slice(b"FAT16   ");
        bpb[510] = 0x55;
        bpb[511] = 0xAA;
    }

    // 4. FAT1 and FAT2
    let mut fat = vec![0u8; SECTORS_PER_FAT * SECTOR];
    let set_fat = |fat: &mut [u8], cluster: usize, val: u16| {
        fat[cluster * 2..cluster * 2 + 2].copy_from_slice(&val.to_le_bytes());
    };
    set_fat(&mut fat, 0, 0xFFF8);
    set_fat(&mut fat, 1, 0xFFFF);
    set_fat(&mut fat, 2, 0xFFFF); // EFI/
    set_fat(&mut fat, 3, 0xFFFF); // EFI/UpdateCapsule/
    for i in 0..file_clusters {
        let cluster = 4 + i;
        let next = if i + 1 < file_clusters {
            (cluster + 1) as u16
        } else {
            0xFFFF
        };
        set_fat(&mut fat, cluster, next);
    }
    for f in 0..NUM_FATS {
        let off = vol_off + (RESERVED_SECTORS + f * SECTORS_PER_FAT) * SECTOR;
        img[off..off + fat.len()].copy_from_slice(&fat);
    }

    let write_sfn = |entry: &mut [u8], name: &[u8; 11], attr: u8, cluster: u16, size: u32| {
        entry[0..11].copy_from_slice(name);
        entry[11] = attr;
        entry[26..28].copy_from_slice(&cluster.to_le_bytes());
        entry[28..32].copy_from_slice(&size.to_le_bytes());
    };

    // 5. Root directory -> EFI (cluster 2)
    let root_off = vol_off + (RESERVED_SECTORS + NUM_FATS * SECTORS_PER_FAT) * SECTOR;
    write_sfn(
        &mut img[root_off..root_off + 32],
        b"EFI        ",
        0x10,
        2,
        0,
    );

    let data_off = root_off + ROOT_DIR_SECTORS * SECTOR;
    let cluster_off = |c: usize| data_off + (c - 2) * CLUSTER_BYTES;

    // 6. Cluster 2: EFI/ -> ".", "..", LFN("UpdateCapsule") + SFN("UPDATE~1   ", cluster 3)
    {
        let c2 = &mut img[cluster_off(2)..cluster_off(2) + CLUSTER_BYTES];
        write_sfn(&mut c2[0..32], b".          ", 0x10, 2, 0);
        write_sfn(&mut c2[32..64], b"..         ", 0x10, 0, 0);

        let sfn: [u8; 11] = *b"UPDATE~1   ";
        let lfn = &mut c2[64..96];
        let chars: Vec<u16> = "UpdateCapsule".encode_utf16().collect();
        assert_eq!(chars.len(), 13);
        lfn[0] = 0x41; // last (and only) LFN entry
        for i in 0..5 {
            lfn[1 + i * 2..3 + i * 2].copy_from_slice(&chars[i].to_le_bytes());
        }
        lfn[11] = 0x0F; // LFN attribute
        lfn[13] = lfn_checksum(&sfn);
        for i in 0..6 {
            lfn[14 + i * 2..16 + i * 2].copy_from_slice(&chars[5 + i].to_le_bytes());
        }
        for i in 0..2 {
            lfn[28 + i * 2..30 + i * 2].copy_from_slice(&chars[11 + i].to_le_bytes());
        }
        write_sfn(&mut c2[96..128], &sfn, 0x10, 3, 0);
    }

    // 7. Cluster 3: EFI/UpdateCapsule/ -> ".", "..", UPDATE.CAP (cluster 4)
    {
        let c3 = &mut img[cluster_off(3)..cluster_off(3) + CLUSTER_BYTES];
        write_sfn(&mut c3[0..32], b".          ", 0x10, 3, 0);
        write_sfn(&mut c3[32..64], b"..         ", 0x10, 2, 0);
        write_sfn(
            &mut c3[64..96],
            b"UPDATE  CAP",
            0x20,
            4,
            u32::try_from(capsule.len()).expect("capsule fits in u32"),
        );
    }

    // 8. Clusters 4.. -> UPDATE.CAP bytes
    let file_off = cluster_off(4);
    img[file_off..file_off + capsule.len()].copy_from_slice(capsule);

    img
}

#[test]
fn esp_disk_builder_produces_valid_gpt_and_fat16_headers() {
    let disk = build_esp_disk_with_capsule(b"hello-capsule");
    assert_eq!(disk.len(), 16_384 * 512);
    assert_eq!(&disk[510..512], &[0x55, 0xAA]);
    assert_eq!(&disk[512..520], b"EFI PART");
    assert_eq!(&disk[2048 * 512 + 54..2048 * 512 + 62], b"FAT16   ");
    assert_eq!(&disk[2048 * 512 + 510..2048 * 512 + 512], &[0x55, 0xAA]);
}

#[test]
#[ignore = "requires qemu-system-x86_64, kvm, and swtpm"]
fn q35_ab_capsule_update_tpm_floor_and_downgrade_rejection() {
    let root = workspace_root().expect("workspace root");
    let manifest = board_manifest::find(&root, "qemu-q35-ab").expect("find qemu-q35-ab manifest");
    let tmp = tempfile::tempdir().expect("tempdir");

    let key_path = tmp.path().join("ed25519.der");
    write_ed25519_pkcs8_der(&key_path, [0x42; 32]);

    let v1 = build_q35_ab(&root, &manifest, &key_path, 1, 1, false);
    let v2 = build_q35_ab(&root, &manifest, &key_path, 2, 2, false);
    // Higher fw_version (3 >= ESRT LSV 1) with lower security_version (1 < TPM floor 2)
    // proves rejection is enforced by the TPM rollback floor + Ed25519 Root preflight,
    // not merely by ESRT LSV.
    let v3_downgrade = build_q35_ab(&root, &manifest, &key_path, 3, 1, false);

    let (rsa_key, cert_der) = dev_capsule_key_and_cert().expect("dev RSA key");
    let v2_ffs = fs::read(&v2.rw_ffs).expect("read v2 ffs");
    let v3_downgrade_ffs = fs::read(&v3_downgrade.rw_ffs).expect("read v3 downgrade ffs");

    let v2_cap = sign_fmp_capsule(
        &CapsuleSpec {
            fw_class_guid: Q35_AB_FIRMWARE_GUID,
            lowest_supported_version: 1,
            monotonic_count: 1,
            rmap_manifest: "RW_SLOT\n",
            slot_size: Q35_AB_RW_A_SIZE as usize,
            rw_slot_image: &v2_ffs,
        },
        &rsa_key,
        &cert_der,
    )
    .expect("sign v2 capsule");

    let v3_downgrade_cap = sign_fmp_capsule(
        &CapsuleSpec {
            fw_class_guid: Q35_AB_FIRMWARE_GUID,
            lowest_supported_version: 1,
            monotonic_count: 2,
            rmap_manifest: "RW_SLOT\n",
            slot_size: Q35_AB_RW_A_SIZE as usize,
            rw_slot_image: &v3_downgrade_ffs,
        },
        &rsa_key,
        &cert_der,
    )
    .expect("sign v3 downgrade capsule");

    let mut tampered_cap = v2_cap.clone();
    let flip_idx = tampered_cap.len() - 64;
    tampered_cap[flip_idx] ^= 0xff;

    let flash = DualPflash::from_full_rom(tmp.path(), &v1.rom);
    flash.seed_nvram_os_indications();

    let esp_v2 = tmp.path().join("esp-v2.img");
    fs::write(&esp_v2, build_esp_disk_with_capsule(&v2_cap)).expect("write esp-v2");

    let tpm = PersistentTpm::create("happy");

    // Boot 1 -> apply v2 capsule -> automatic cold reset -> Boot 2 (Slot B trial + confirm + sync)
    let log12 = run_qemu_until(
        &flash,
        Some(&tpm),
        Some(&esp_v2),
        true,
        "Boot manager finished",
        1,
        Duration::from_secs(120),
    );
    for expected in [
        "qemu-q35-ab ro: provisioned TPM rollback floor = 1",
        "qemu-q35-ab ro: TPM rollback floor locked",
        "qemu-q35-ab ro: selected slot A fw=1 sec=1 floor=1 trial=0 sync=0",
        "qemu-q35-ab ramstage: OsIndications requests Capsule-on-Disk; deferring flash lock",
        "File-based capsule delivery requested via OsIndications",
        "Loaded capsule file: UPDATE.CAP",
        "Applied 1 pending firmware capsule(s)",
        "qemu-q35-ab ro: selected slot B fw=2 sec=2 floor=1 trial=1 sync=0",
        "qemu-q35-ab ramstage: trial confirmed (slot=B fw=2 sec=2)",
        "qemu-q35-ab ramstage: flash write-locked for normal boot",
        "ESRT installed: version=0x2, LSV=0x1",
    ] {
        assert!(
            log12.contains(expected),
            "Boot 1+2 log missing `{expected}`.\n--- serial log ---\n{log12}"
        );
    }

    // Boot 3 -> RO verifies both synced slots, advances TPM floor 1 -> 2, finalizes to Stable.
    let log3 = run_qemu_until(
        &flash,
        Some(&tpm),
        None,
        false,
        "Boot manager finished",
        1,
        Duration::from_secs(90),
    );
    for expected in [
        "qemu-q35-ab ro: TPM rollback floor = 1",
        "qemu-q35-ab ro: TPM rollback floor advanced to 2",
        "qemu-q35-ab ro: slot state finalized to Stable",
        "qemu-q35-ab ro: TPM rollback floor locked",
        "qemu-q35-ab ro: selected slot B fw=2 sec=2 floor=2 trial=0 sync=0",
        "qemu-q35-ab ramstage: flash write-locked for normal boot",
        "ESRT installed: version=0x2, LSV=0x1",
    ] {
        assert!(
            log3.contains(expected),
            "Boot 3 log missing `{expected}`.\n--- serial log ---\n{log3}"
        );
    }

    // Boot 4 -> Downgrade attempt with validly-signed fw=3, sec=1 (< floor=2) must be rejected.
    flash.seed_nvram_os_indications();
    let esp_downgrade = tmp.path().join("esp-downgrade.img");
    fs::write(
        &esp_downgrade,
        build_esp_disk_with_capsule(&v3_downgrade_cap),
    )
    .expect("write esp-downgrade");
    let log4 = run_qemu_until(
        &flash,
        Some(&tpm),
        Some(&esp_downgrade),
        false,
        "Boot manager finished",
        1,
        Duration::from_secs(90),
    );
    for expected in [
        "qemu-q35-ab ro: TPM rollback floor = 2",
        "qemu-q35-ab ro: selected slot B fw=2 sec=2 floor=2 trial=0 sync=0",
        "File-based capsule delivery requested via OsIndications",
        "Loaded capsule file: UPDATE.CAP",
        "fstart capsule: rejected RW Root preflight (floor=2, err=PolicyRejected)",
    ] {
        assert!(
            log4.contains(expected),
            "Boot 4 log missing `{expected}`.\n--- serial log ---\n{log4}"
        );
    }
    assert!(
        !log4.contains("Applied 1 pending firmware capsule"),
        "Downgrade capsule (fw=3, sec=1) MUST be rejected when TPM floor == 2!\n--- serial log ---\n{log4}"
    );

    // Boot 5 -> Tampered capsule must be rejected by PKCS#7 verification without touching slots.
    flash.seed_nvram_os_indications();
    let esp_tampered = tmp.path().join("esp-tampered.img");
    fs::write(&esp_tampered, build_esp_disk_with_capsule(&tampered_cap))
        .expect("write esp-tampered");
    let log5 = run_qemu_until(
        &flash,
        Some(&tpm),
        Some(&esp_tampered),
        false,
        "Boot manager finished",
        1,
        Duration::from_secs(90),
    );
    for expected in [
        "File-based capsule delivery requested via OsIndications",
        "Loaded capsule file: UPDATE.CAP",
    ] {
        assert!(
            log5.contains(expected),
            "Boot 5 log missing `{expected}`.\n--- serial log ---\n{log5}"
        );
    }
    assert!(
        !log5.contains("Applied 1 pending firmware capsule"),
        "Tampered capsule MUST be rejected!\n--- serial log ---\n{log5}"
    );

    match flash.inspect_slot_state() {
        ScanStatus::Valid { record, .. } => {
            assert_eq!(record.phase, Phase::Stable);
            assert_eq!(record.active_slot, Slot::B);
            assert_eq!(record.fw_version, 2);
            assert_eq!(record.sec_version, 2);
        }
        other => panic!("expected Valid Stable Slot B v2 after rejections, got {other:?}"),
    }
    flash.assert_ro_unmodified();
}

#[test]
#[ignore = "requires qemu-system-x86_64, kvm, and swtpm"]
fn q35_ab_broken_trial_exhausts_budget_and_falls_back() {
    let root = workspace_root().expect("workspace root");
    let manifest = board_manifest::find(&root, "qemu-q35-ab").expect("find qemu-q35-ab manifest");
    let tmp = tempfile::tempdir().expect("tempdir");

    let key_path = tmp.path().join("ed25519.der");
    write_ed25519_pkcs8_der(&key_path, [0x77; 32]);

    let v1 = build_q35_ab(&root, &manifest, &key_path, 1, 1, false);
    let v2_broken = build_q35_ab(&root, &manifest, &key_path, 2, 2, true);

    let (rsa_key, cert_der) = dev_capsule_key_and_cert().expect("dev RSA key");
    let v2_ffs = fs::read(&v2_broken.rw_ffs).expect("read broken v2 ffs");
    let v2_cap = sign_fmp_capsule(
        &CapsuleSpec {
            fw_class_guid: Q35_AB_FIRMWARE_GUID,
            lowest_supported_version: 1,
            monotonic_count: 1,
            rmap_manifest: "RW_SLOT\n",
            slot_size: Q35_AB_RW_A_SIZE as usize,
            rw_slot_image: &v2_ffs,
        },
        &rsa_key,
        &cert_der,
    )
    .expect("sign broken v2 capsule");

    let flash = DualPflash::from_full_rom(tmp.path(), &v1.rom);
    flash.seed_nvram_os_indications();

    let esp_v2 = tmp.path().join("esp-v2-broken.img");
    fs::write(&esp_v2, build_esp_disk_with_capsule(&v2_cap)).expect("write esp-v2-broken");

    let tpm = PersistentTpm::create("rollback");

    // Boot 1 (apply broken v2) -> Boots 2, 3, 4 (3 trial resets) -> Boot 5 (fallback to Slot A).
    let log = run_qemu_until(
        &flash,
        Some(&tpm),
        Some(&esp_v2),
        true,
        "Boot manager finished",
        1,
        Duration::from_secs(180),
    );

    let reset_count = log
        .matches("simulating broken trial before confirmation -> cold reset")
        .count();
    assert_eq!(
        reset_count, 3,
        "expected exactly 3 broken trial resets before fallback, got {reset_count}.\n--- serial log ---\n{log}"
    );
    for expected in [
        "Applied 1 pending firmware capsule(s)",
        "qemu-q35-ab ro: selected slot B fw=2 sec=2 floor=1 trial=1 sync=0",
        "qemu-q35-ab ramstage: slot=A fw=1 sec=1 floor=1 trial=0 sync=0 last_ver=2 last_status=1",
    ] {
        assert!(
            log.contains(expected),
            "Rollback log missing `{expected}`.\n--- serial log ---\n{log}"
        );
    }

    // Boot 6 -> Subsequent cold boot stays cleanly on Slot A (Stable, floor still 1).
    let log6 = run_qemu_until(
        &flash,
        Some(&tpm),
        None,
        false,
        "Boot manager finished",
        1,
        Duration::from_secs(90),
    );
    for expected in [
        "qemu-q35-ab ro: TPM rollback floor = 1",
        "qemu-q35-ab ro: selected slot A fw=1 sec=1 floor=1 trial=0 sync=0",
        "qemu-q35-ab ramstage: slot=A fw=1 sec=1 floor=1 trial=0 sync=0 last_ver=2 last_status=1",
    ] {
        assert!(
            log6.contains(expected),
            "Post-rollback cold boot log missing `{expected}`.\n--- serial log ---\n{log6}"
        );
    }

    // Verify flash journal state directly.
    match flash.inspect_slot_state() {
        ScanStatus::Valid { record, .. } => {
            assert_eq!(record.phase, Phase::Stable);
            assert_eq!(record.active_slot, Slot::A);
            assert_eq!(record.fw_version, 1);
            assert_eq!(record.sec_version, 1);
            assert_eq!(record.last_attempt_ver, 2);
            assert_eq!(record.last_attempt_status, 1);
        }
        other => panic!("expected Valid Stable Slot A after fallback, got {other:?}"),
    }
    flash.assert_ro_unmodified();
}

#[test]
#[ignore = "requires qemu-system-x86_64 and kvm"]
fn q35_ab_halts_fail_closed_without_tpm() {
    let root = workspace_root().expect("workspace root");
    let manifest = board_manifest::find(&root, "qemu-q35-ab").expect("find qemu-q35-ab manifest");
    let tmp = tempfile::tempdir().expect("tempdir");

    let key_path = tmp.path().join("ed25519.der");
    write_ed25519_pkcs8_der(&key_path, [0x99; 32]);

    let v1 = build_q35_ab(&root, &manifest, &key_path, 1, 1, false);
    let flash = DualPflash::from_full_rom(tmp.path(), &v1.rom);

    let log = run_qemu_until(
        &flash,
        None,
        None,
        false,
        "qemu-q35-ab ro: required FIFO TPM 2.0 absent",
        1,
        Duration::from_secs(30),
    );
    assert!(
        !log.contains("qemu-q35-ab ro: selected slot"),
        "RO must halt fail-closed without selecting or booting a slot when TPM is absent!\n--- serial log ---\n{log}"
    );
    flash.assert_ro_unmodified();
}
