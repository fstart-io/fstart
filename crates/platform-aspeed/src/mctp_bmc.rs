//! Bare-metal AST2500 BMC MCTP + PLDM + SPDM responder over UART1
//! (`0x1E78_3000`, wired to QEMU's second `-serial` chardev on `ast2500-evb`).

use fstart_core::services::Console;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_mctp::binding::serial::{self, SerialDecoder};
use fstart_mctp::control::ControlResponder;
use fstart_mctp::{
    BASELINE_MTU, Endpoint, Error as MctpError, MctpChannel, MessageType, PacketTransport,
};
use fstart_pldm::PldmResponder;
use fstart_pldm::platform::NumericSensor;
use fstart_spdm::crypto::DeterministicRng;
use fstart_spdm::{SecuredMctpChannel, SessionTransport, SpdmKeyPair, SpdmResponder};

/// AST2500 UART1 MMIO base (separate from UART5 debug console at `0x1E78_4000`).
const AST2500_UART1_BASE: u64 = 0x1E78_3000;

struct AspeedUartTransport {
    uart: Ns16550,
    decoder: SerialDecoder,
    rx_polls: u32,
}

impl PacketTransport for AspeedUartTransport {
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

/// Wait briefly on AST2500 UART1 for an MCTP Serial sync byte (`0x7E`); if a
/// host peer starts talking, serve one full MCTP Control + SPDM 1.2 +
/// PLDM-over-Secured-MCTP + END_SESSION session and return.
pub fn serve_if_present() {
    let cfg = Ns16550Config {
        regs: AccessMode::Mmio {
            base: AST2500_UART1_BASE,
            reg_shift: 2,
            reg_width: 4,
        },
        clock_freq: 24_000_000,
        baud_rate: 115_200,
    };
    let Ok(mut uart) = Ns16550::new(cfg) else {
        return;
    };
    if uart.init().is_err() {
        return;
    }

    // Brief probe for an incoming MCTP serial frame (0x7E).
    let mut decoder = SerialDecoder::new();
    let mut first_pkt = [0u8; 320];
    let mut first_len = 0usize;
    for _ in 0..2_000_000u32 {
        if let Ok(Some(b)) = uart.read_byte()
            && let Ok(Some(pkt)) = decoder.push_byte(b)
        {
            first_len = pkt.len();
            first_pkt[..first_len].copy_from_slice(pkt);
            break;
        }
        core::hint::spin_loop();
    }
    if first_len == 0 {
        return;
    }

    fstart_log::info!("ast2500-bmc: MCTP-SERIAL peer detected on UART1");
    let transport = AspeedUartTransport {
        uart,
        decoder,
        rx_polls: 100_000_000,
    };
    let mut ep = Endpoint::<_, 512, 2>::new(
        transport,
        ControlResponder::new(0x00, [0xAA; 16], &[0x01, 0x05, 0x06]),
    );

    // Handle the first packet: require a valid MCTP Control SetEndpointId request.
    let Ok((hdr, body)) = fstart_mctp::MctpHeader::parse(&first_pkt[..first_len]) else {
        return;
    };
    let Some((&type_byte, ctrl_req)) = body.split_first() else {
        return;
    };
    if (type_byte & 0x7F) != MessageType::Control as u8 || !hdr.tag_owner {
        return;
    }
    let mut ctrl_resp = [0u8; 64];
    let Ok(rlen) = ep.control_mut().handle_request(ctrl_req, &mut ctrl_resp) else {
        return;
    };
    if rlen == 0 {
        return;
    }
    let _ = ep.send_message(
        hdr.src_eid,
        MessageType::Control as u8,
        Some(hdr.msg_tag),
        &ctrl_resp[..rlen],
    );

    match serve_session(&mut ep) {
        Ok(()) => fstart_log::info!("ast2500-bmc: MCTP+PLDM+SPDM-SERIAL PASS"),
        Err(step) => fstart_log::warn!("ast2500-bmc: MCTP+PLDM+SPDM-SERIAL failed at {}", step),
    }
}

fn serve_session(ep: &mut Endpoint<AspeedUartTransport, 512, 2>) -> Result<(), &'static str> {
    let keypair = SpdmKeyPair::from_secret_bytes(&[0x55; 32]).map_err(|_| "keypair")?;
    let mut spdm =
        SpdmResponder::<512, 2>::new(SessionTransport::Mctp, keypair, b"ast2500-bmc-cert")
            .map_err(|_| "spdm_new")?;
    let mut pldm = PldmResponder::<4, 2, 512>::new(0x21, 0x9E9E);
    pldm.platform
        .add_sensor(NumericSensor {
            sensor_id: 1,
            reading: 61_200,
        })
        .map_err(|_| "add_sensor")?;
    let mut rng = DeterministicRng::new(0x9999);
    let mut rx = [0u8; 512];
    let mut resp = [0u8; 512];

    // 5 plaintext SPDM requests (VCA + DIGESTS + KEY_EXCHANGE); GetEID Control
    // is handled automatically inside recv_handling_control!
    for _ in 0..5 {
        let m = ep.recv_handling_control(&mut rx).map_err(|_| "rx_spdm")?;
        if m.msg_type != MessageType::Spdm as u8 {
            return Err("not_spdm");
        }
        let rlen = spdm
            .handle_request(m.payload, &mut resp, &mut rng)
            .map_err(|_| "h_spdm")?;
        ep.send_message(
            m.src_eid,
            MessageType::Spdm as u8,
            Some(m.msg_tag),
            &resp[..rlen],
        )
        .map_err(|_| "tx_spdm")?;
    }

    // Encrypted FINISH
    let m = ep.recv_handling_control(&mut rx).map_err(|_| "rx_fin")?;
    let rlen = spdm
        .handle_secured_spdm(m.payload, &mut resp)
        .map_err(|_| "h_fin")?;
    ep.send_message(
        m.src_eid,
        MessageType::SecuredMsg as u8,
        Some(m.msg_tag),
        &resp[..rlen],
    )
    .map_err(|_| "tx_fin")?;

    // 2 PLDM requests over SPDM-Secured MCTP
    {
        let mut sec = SecuredMctpChannel {
            inner: ep,
            session: spdm.session.as_mut().ok_or("no_sess")?,
        };
        for _ in 0..2 {
            let m = sec.recv_message(&mut rx).map_err(|_| "rx_pldm")?;
            if m.msg_type != MessageType::Pldm as u8 {
                return Err("not_pldm");
            }
            let rlen = pldm
                .handle_request(m.payload, &mut resp)
                .map_err(|_| "h_pldm")?;
            sec.send_message(
                m.src_eid,
                MessageType::Pldm as u8,
                Some(m.msg_tag),
                &resp[..rlen],
            )
            .map_err(|_| "tx_pldm")?;
        }
    }

    // Encrypted END_SESSION
    let m = ep.recv_handling_control(&mut rx).map_err(|_| "rx_end")?;
    let rlen = spdm
        .handle_secured_spdm(m.payload, &mut resp)
        .map_err(|_| "h_end")?;
    ep.send_message(
        m.src_eid,
        MessageType::SecuredMsg as u8,
        Some(m.msg_tag),
        &resp[..rlen],
    )
    .map_err(|_| "tx_end")?;
    Ok(())
}
