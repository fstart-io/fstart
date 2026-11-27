//! Host-side QEMU q35 validation of SPDM over PCIe DOE and MCTP + PLDM + SPDM
//! over Serial (COM2 `0x2F8`), exercised directly from bare-metal x86_64
//! firmware.

use fstart_core::services::Console;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_mctp::binding::serial::{self, SerialDecoder};
use fstart_mctp::control::{
    ControlResponder, SetEidOperation, encode_get_eid_req, encode_set_eid_req,
};
use fstart_mctp::{
    BASELINE_MTU, Endpoint, Error as MctpError, MctpChannel, MessageType, PacketTransport,
};
use fstart_pci::doe::{DoeProtocol, doe_exchange, find_doe_capability};
use fstart_pci::{PciAddress, PciEcam};
use fstart_pldm::base::encode_get_tid_req;
use fstart_pldm::platform::encode_get_sensor_reading_req;
use fstart_pmci::SecuredMctpChannel;
use fstart_spdm::crypto::DeterministicRng;
use fstart_spdm::{SessionTransport, SpdmCode, SpdmKeyPair, SpdmRequester};

/// Run PCIe DOE SPDM validation (if a DOE-capable PCIe device is attached) and
/// Serial MCTP+PLDM+SPDM validation (if a BMC responder is attached to COM2).
pub fn run(ecam: &PciEcam) {
    validate_pcie_doe(ecam);
    validate_serial_mctp();
}

/// Scan PCI bus 0 for a DOE extended capability (e.g. QEMU `-device nvme,spdm_port=...`)
/// and run a complete SPDM 1.2 lifecycle over PCIe DOE.
fn validate_pcie_doe(ecam: &PciEcam) {
    for dev in 0..32u8 {
        let addr = PciAddress::new(0, 0, dev, 0);
        let Some(cap_off) = find_doe_capability(ecam, addr) else {
            continue;
        };
        fstart_log::info!(
            "qemu-q35: found PCIe DOE cap at 00:{:#x}.0 offset {:#x}",
            dev,
            cap_off
        );
        match run_doe_spdm_handshake(ecam, addr, cap_off) {
            Ok(()) => fstart_log::info!("qemu-q35: SPDM-DOE PASS"),
            Err(step) => fstart_log::warn!("qemu-q35: SPDM-DOE failed at {}", step),
        }
        return;
    }
}

fn run_doe_spdm_handshake(
    ecam: &PciEcam,
    addr: PciAddress,
    cap_off: u16,
) -> Result<(), &'static str> {
    let keypair = SpdmKeyPair::from_secret_bytes(&[0x42; 32]).map_err(|_| "keypair")?;
    let pub_xy = *keypair.public_xy();
    let mut req = SpdmRequester::<512>::new(SessionTransport::Doe, pub_xy);
    let mut rng = DeterministicRng::new(0x1234_5678);
    let mut tx = [0u8; 512];
    let mut rx = [0u8; 512];
    const POLLS: u32 = 2_000_000;

    let n = req.build_get_version(&mut tx).map_err(|_| "build_ver")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_ver")?;
    req.handle_version(&rx[..r]).map_err(|_| "handle_ver")?;

    let n = req
        .build_get_capabilities(&mut tx)
        .map_err(|_| "build_cap")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_cap")?;
    req.handle_capabilities(&rx[..r])
        .map_err(|_| "handle_cap")?;

    let n = req
        .build_negotiate_algorithms(&mut tx)
        .map_err(|_| "build_alg")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_alg")?;
    req.handle_algorithms(&rx[..r]).map_err(|_| "handle_alg")?;

    let n = req.build_get_digests(&mut tx).map_err(|_| "build_dig")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_dig")?;
    req.handle_digests(&rx[..r]).map_err(|_| "handle_dig")?;

    let mut offset = 0u16;
    let mut cert_done = false;
    for _ in 0..32 {
        let n = req
            .build_get_certificate(offset, 64, &mut tx)
            .map_err(|_| "build_cert")?;
        let r = doe_exchange(
            ecam,
            addr,
            cap_off,
            DoeProtocol::CMA_SPDM,
            &tx[..n],
            &mut rx,
            POLLS,
        )
        .map_err(|_| "doe_cert")?;
        if r < 8 {
            return Err("short_cert");
        }
        let portion = u16::from_le_bytes([rx[4], rx[5]]);
        if portion == 0 {
            return Err("zero_cert_portion");
        }
        let rem = req
            .handle_certificate(&rx[..r])
            .map_err(|_| "handle_cert")?;
        offset = offset.checked_add(portion).ok_or("cert_overflow")?;
        if rem == 0 {
            cert_done = true;
            break;
        }
    }
    if !cert_done {
        return Err("cert_loop_limit");
    }

    let n = req
        .build_challenge(&mut rng, &mut tx)
        .map_err(|_| "build_chal")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_chal")?;
    req.verify_challenge_auth(&tx[..n], &rx[..r])
        .map_err(|_| "verify_chal")?;

    let n = req
        .build_get_measurements(&mut rng, &mut tx)
        .map_err(|_| "build_meas")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_meas")?;
    req.verify_measurements(&tx[..n], &rx[..r])
        .map_err(|_| "verify_meas")?;

    let n = req
        .build_key_exchange(&mut rng, &mut tx)
        .map_err(|_| "build_kx")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_kx")?;
    req.handle_key_exchange_rsp(&tx[..n], &rx[..r])
        .map_err(|_| "handle_kx")?;

    let n = req.build_secured_finish(&mut tx).map_err(|_| "build_fin")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::SECURED_CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_fin")?;
    req.handle_secured_finish_rsp(&rx[..r])
        .map_err(|_| "handle_fin")?;

    let n = req
        .build_secured_cmd(SpdmCode::Heartbeat, 0, 0, &mut tx)
        .map_err(|_| "build_hb")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::SECURED_CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_hb")?;
    req.handle_secured_ack(SpdmCode::HeartbeatAck, &rx[..r])
        .map_err(|_| "handle_hb")?;

    let n = req
        .build_secured_cmd(SpdmCode::EndSession, 0, 0, &mut tx)
        .map_err(|_| "build_end")?;
    let r = doe_exchange(
        ecam,
        addr,
        cap_off,
        DoeProtocol::SECURED_CMA_SPDM,
        &tx[..n],
        &mut rx,
        POLLS,
    )
    .map_err(|_| "doe_end")?;
    req.handle_secured_ack(SpdmCode::EndSessionAck, &rx[..r])
        .map_err(|_| "handle_end")?;
    Ok(())
}

/// Polled DSP0253 serial transport over an `Ns16550` UART.
struct UartMctpTransport {
    uart: Ns16550,
    decoder: SerialDecoder,
    rx_polls: u32,
}

impl PacketTransport for UartMctpTransport {
    fn mtu(&self) -> usize {
        BASELINE_MTU
    }

    fn send_packet(&mut self, pkt: &[u8]) -> Result<(), MctpError> {
        let mut frame = [0u8; 320];
        let n = serial::encode(pkt, &mut frame)?;
        for &b in &frame[..n] {
            self.uart.write_byte(b).map_err(|_| MctpError::Transport)?;
        }
        let _ = self.uart.flush();
        Ok(())
    }

    fn recv_packet(&mut self, out: &mut [u8]) -> Result<usize, MctpError> {
        for _ in 0..self.rx_polls {
            if let Ok(Some(b)) = self.uart.read_byte()
                && let Ok(Some(pkt)) = self.decoder.push_byte(b)
            {
                if out.len() < pkt.len() {
                    return Err(MctpError::BufferTooSmall);
                }
                out[..pkt.len()].copy_from_slice(pkt);
                return Ok(pkt.len());
            }
            core::hint::spin_loop();
        }
        Err(MctpError::Timeout)
    }
}

/// Probe COM2 (`0x2F8`) for an attached BMC MCTP peer; if present, run full
/// MCTP Control + SPDM 1.2 + PLDM-over-Secured-MCTP + END_SESSION.
fn validate_serial_mctp() {
    let cfg = Ns16550Config {
        regs: AccessMode::Pio { base: 0x2F8 },
        clock_freq: 1_843_200,
        baud_rate: 115_200,
    };
    let Ok(mut uart) = Ns16550::new(cfg) else {
        return;
    };
    if uart.init().is_err() {
        return;
    }
    // Drain any stale RX bytes (bounded).
    for _ in 0..64 {
        match uart.read_byte() {
            Ok(Some(_)) => {}
            _ => break,
        }
    }

    // Short initial probe budget so normal boots without COM2 skip quickly;
    // once SetEID succeeds, switch to a generous budget for SPDM crypto on TCG.
    let transport = UartMctpTransport {
        uart,
        decoder: SerialDecoder::new(),
        rx_polls: 400_000,
    };
    let mut ep = Endpoint::<_, 512, 2>::new(
        transport,
        ControlResponder::new(0x08, [0xBB; 16], &[0x01, 0x05, 0x06]).with_bus_owner(true),
    );

    let mut tx = [0u8; 256];
    let mut rx = [0u8; 512];
    let Ok(n) = encode_set_eid_req(1, SetEidOperation::SetEid, 0x0A, &mut tx) else {
        return;
    };
    if ep
        .send_message(0x00, MessageType::Control as u8, None, &tx[..n])
        .is_err()
    {
        return;
    }
    let Ok(reply) = ep.recv_message(&mut rx) else {
        // No MCTP peer attached on COM2.
        return;
    };
    if reply.payload.len() < 5 || reply.payload[4] != 0x0A {
        fstart_log::warn!("qemu-q35: MCTP-SERIAL SetEID unexpected reply");
        return;
    }
    fstart_log::info!("qemu-q35: MCTP-SERIAL BMC peer discovered (EID=0x0A)");
    // Generous budget for ARM1176 TCG P-256 ECDSA/ECDHE on the BMC VM.
    ep.transport_mut().rx_polls = 200_000_000;

    match run_serial_mctp_pldm_spdm(&mut ep) {
        Ok(()) => fstart_log::info!("qemu-q35: MCTP+PLDM+SPDM-SERIAL PASS"),
        Err(step) => fstart_log::warn!("qemu-q35: MCTP+PLDM+SPDM-SERIAL failed at {}", step),
    }
}

fn run_serial_mctp_pldm_spdm(
    ep: &mut Endpoint<UartMctpTransport, 512, 2>,
) -> Result<(), &'static str> {
    let keypair = SpdmKeyPair::from_secret_bytes(&[0x55; 32]).map_err(|_| "keypair")?;
    let pub_xy = *keypair.public_xy();
    let mut spdm = SpdmRequester::<512>::new(SessionTransport::Mctp, pub_xy);
    let mut rng = DeterministicRng::new(0x7777);
    let mut tx = [0u8; 256];
    let mut rx = [0u8; 512];

    // GetEID
    let n = encode_get_eid_req(2, &mut tx).map_err(|_| "enc_get_eid")?;
    ep.send_message(0x0A, MessageType::Control as u8, None, &tx[..n])
        .map_err(|_| "tx_get_eid")?;
    let m = ep.recv_message(&mut rx).map_err(|_| "rx_get_eid")?;
    if m.payload.len() < 4 || m.payload[3] != 0x0A {
        return Err("bad_get_eid");
    }

    // SPDM 1.2 handshake
    let n = spdm.build_get_version(&mut tx).map_err(|_| "b_ver")?;
    ep.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
        .map_err(|_| "tx_ver")?;
    spdm.handle_version(ep.recv_message(&mut rx).map_err(|_| "rx_ver")?.payload)
        .map_err(|_| "h_ver")?;

    let n = spdm.build_get_capabilities(&mut tx).map_err(|_| "b_cap")?;
    ep.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
        .map_err(|_| "tx_cap")?;
    spdm.handle_capabilities(ep.recv_message(&mut rx).map_err(|_| "rx_cap")?.payload)
        .map_err(|_| "h_cap")?;

    let n = spdm
        .build_negotiate_algorithms(&mut tx)
        .map_err(|_| "b_alg")?;
    ep.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
        .map_err(|_| "tx_alg")?;
    spdm.handle_algorithms(ep.recv_message(&mut rx).map_err(|_| "rx_alg")?.payload)
        .map_err(|_| "h_alg")?;

    let n = spdm.build_get_digests(&mut tx).map_err(|_| "b_dig")?;
    ep.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
        .map_err(|_| "tx_dig")?;
    spdm.handle_digests(ep.recv_message(&mut rx).map_err(|_| "rx_dig")?.payload)
        .map_err(|_| "h_dig")?;

    let n = spdm
        .build_key_exchange(&mut rng, &mut tx)
        .map_err(|_| "b_kx")?;
    ep.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
        .map_err(|_| "tx_kx")?;
    spdm.handle_key_exchange_rsp(
        &tx[..n],
        ep.recv_message(&mut rx).map_err(|_| "rx_kx")?.payload,
    )
    .map_err(|_| "h_kx")?;

    let n = spdm.build_secured_finish(&mut tx).map_err(|_| "b_fin")?;
    ep.send_message(0x0A, MessageType::SecuredMsg as u8, None, &tx[..n])
        .map_err(|_| "tx_fin")?;
    spdm.handle_secured_finish_rsp(ep.recv_message(&mut rx).map_err(|_| "rx_fin")?.payload)
        .map_err(|_| "h_fin")?;

    // PLDM over SPDM-Secured MCTP (0x06)
    {
        let mut sec = SecuredMctpChannel {
            inner: ep,
            session: spdm.session.as_mut().ok_or("no_sess")?,
        };
        let n = encode_get_tid_req(1, &mut tx).map_err(|_| "b_tid")?;
        sec.send_message(0x0A, MessageType::Pldm as u8, None, &tx[..n])
            .map_err(|_| "tx_tid")?;
        let m = sec.recv_message(&mut rx).map_err(|_| "rx_tid")?;
        if m.payload.len() < 5 || m.payload[4] != 0x21 {
            return Err("bad_tid");
        }

        let n = encode_get_sensor_reading_req(2, 1, &mut tx).map_err(|_| "b_sens")?;
        sec.send_message(0x0A, MessageType::Pldm as u8, None, &tx[..n])
            .map_err(|_| "tx_sens")?;
        let m = sec.recv_message(&mut rx).map_err(|_| "rx_sens")?;
        if m.payload.len() < 14 {
            return Err("short_sens");
        }
        let reading =
            u32::from_le_bytes([m.payload[10], m.payload[11], m.payload[12], m.payload[13]]);
        if reading != 61_200 {
            return Err("bad_sens");
        }
    }

    // END_SESSION
    let n = spdm
        .build_secured_cmd(SpdmCode::EndSession, 0, 0, &mut tx)
        .map_err(|_| "b_end")?;
    ep.send_message(0x0A, MessageType::SecuredMsg as u8, None, &tx[..n])
        .map_err(|_| "tx_end")?;
    spdm.handle_secured_ack(
        SpdmCode::EndSessionAck,
        ep.recv_message(&mut rx).map_err(|_| "rx_end")?.payload,
    )
    .map_err(|_| "h_end")?;
    Ok(())
}
