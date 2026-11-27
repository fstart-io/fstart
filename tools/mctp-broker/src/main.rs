//! Multi-transport MCTP/PLDM/SPDM broker, Wireshark `pcapng`
//! (`LINKTYPE_LINUX_SLL2` / `ETH_P_MCTP`) tap, QEMU `spdm-emu` DOE server,
//! and bare-metal QEMU validation runner.

mod pcapng;
mod qemu_validate;

use clap::{Parser, Subcommand, ValueEnum};
use fstart_mctp::binding::{i3c, kcs, pcie_vdm, serial, smbus};
use fstart_mctp::control::{
    ControlResponder, SetEidOperation, encode_get_eid_req, encode_set_eid_req,
};
use fstart_mctp::{
    BASELINE_MTU, Endpoint, Error as MctpError, MctpChannel, MessageType, PacketTransport,
    TransportKind,
};
use fstart_pldm::PldmResponder;
use fstart_pldm::base::encode_get_tid_req;
use fstart_pldm::platform::{NumericSensor, encode_get_sensor_reading_req};
use fstart_pmci::SecuredMctpChannel;
use fstart_spdm::crypto::{DeterministicRng, sha256};
use fstart_spdm::{
    MeasurementBlock, SessionTransport, SpdmCode, SpdmKeyPair, SpdmRequester, SpdmResponder,
};
use pcapng::PcapNgWriter;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use zerocopy::big_endian::U32 as BeU32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SpdmEmuHeader {
    cmd: BeU32,
    transport_type: BeU32,
    size: BeU32,
}

#[derive(Parser)]
#[command(name = "fstart-mctp-broker")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum CliTransport {
    Serial,
    Smbus,
    I3c,
    Kcs,
    PcieVdm,
}

impl From<CliTransport> for TransportKind {
    fn from(v: CliTransport) -> Self {
        match v {
            CliTransport::Serial => Self::Serial,
            CliTransport::Smbus => Self::Smbus,
            CliTransport::I3c => Self::I3c,
            CliTransport::Kcs => Self::Kcs,
            CliTransport::PcieVdm => Self::PcieVdm,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Bridge two QEMU chardev Unix sockets (e.g. BMC UART <-> Host UART) and
    /// record every MCTP packet to a Wireshark pcapng file (SLL2 / ETH_P_MCTP).
    Bridge {
        #[arg(long)]
        bmc_sock: PathBuf,
        #[arg(long)]
        host_sock: PathBuf,
        #[arg(long, value_enum, default_value = "serial")]
        transport: CliTransport,
        #[arg(long)]
        pcap: Option<PathBuf>,
    },
    /// Run a TCP SPDM-over-DOE responder speaking the QEMU `spdm-emu` socket
    /// protocol (compatible with `qemu-system-x86_64 -device nvme,spdm_port=...`).
    SpdmEmu {
        #[arg(long, default_value_t = 2323)]
        port: u16,
    },
    /// Run an end-to-end BMC <-> Host validation over all 5 bindings (Serial,
    /// SMBus, I3C, KCS, PCIe VDM) and emit a Wireshark pcapng capture.
    SelfTest {
        #[arg(long)]
        pcap: PathBuf,
    },
    /// Run bare-metal QEMU validation: Host qemu-q35 PCIe DOE + Dual-QEMU
    /// ast2500-evb BMC <-> qemu-q35 Host Serial MCTP/PLDM/SPDM.
    QemuValidate {
        #[arg(long)]
        q35_bios: Option<PathBuf>,
        #[arg(long)]
        bmc_flash: Option<PathBuf>,
        #[arg(long, default_value = "/tmp/dual-qemu-serial.pcapng")]
        pcap: PathBuf,
    },
}

// DMTF spdm-emu socket protocol constants (compatible with QEMU hw/misc/spdm-socket.c).
const SOCKET_SPDM_COMMAND_NORMAL: u32 = 0x0001;
const SOCKET_SPDM_COMMAND_STOP: u32 = 0xFFFE;
const SOCKET_SPDM_COMMAND_TEST: u32 = 0xDEAD;
const SOCKET_TRANSPORT_TYPE_PCI_DOE: u32 = 0x02;

fn handle_spdm_emu_client(mut stream: TcpStream) -> io::Result<()> {
    let keypair = SpdmKeyPair::from_secret_bytes(&[0x42; 32]).expect("valid static P-256 key");
    let mut rsp =
        SpdmResponder::<512, 4>::new(SessionTransport::Doe, keypair, b"fstart-qemu-doe-spdm-cert")
            .expect("responder init");
    let _ = rsp.add_measurement(MeasurementBlock {
        index: 1,
        value_type: 0x00,
        digest: sha256(b"fstart-nvme-doe-fw-v1"),
    });
    let mut rng = DeterministicRng::new(0xD0E1_2345);
    let mut out_buf = [0u8; 4096];

    loop {
        let mut hdr_bytes = [0u8; core::mem::size_of::<SpdmEmuHeader>()];
        if let Err(e) = stream.read_exact(&mut hdr_bytes) {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                return Ok(());
            }
            return Err(e);
        }
        let hdr = SpdmEmuHeader::read_from_bytes(&hdr_bytes).unwrap();
        let cmd = hdr.cmd.get();
        let transport_type = hdr.transport_type.get();
        let size = hdr.size.get() as usize;
        if size > 65536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized frame",
            ));
        }
        let mut payload = vec![0u8; size];
        stream.read_exact(&mut payload)?;

        match cmd {
            SOCKET_SPDM_COMMAND_STOP => {
                send_emu_frame(&mut stream, SOCKET_SPDM_COMMAND_STOP, transport_type, &[])?;
                return Ok(());
            }
            SOCKET_SPDM_COMMAND_TEST => {
                send_emu_frame(
                    &mut stream,
                    SOCKET_SPDM_COMMAND_TEST,
                    transport_type,
                    b"Server Hello!",
                )?;
            }
            SOCKET_SPDM_COMMAND_NORMAL => {
                // PCIe DOE payload starts with 8-byte DOE header (vendor_id, type, length_dws).
                if transport_type == SOCKET_TRANSPORT_TYPE_PCI_DOE && payload.len() >= 8 {
                    let doe_vid = u16::from_le_bytes([payload[0], payload[1]]);
                    let doe_type = payload[2];
                    if doe_vid != 0x0001 || (doe_type != 0x01 && doe_type != 0x02) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected DOE protocol",
                        ));
                    }
                    let spdm_in = &payload[8..];
                    let rlen = match if doe_type == 0x02 {
                        rsp.handle_secured_spdm(spdm_in, &mut out_buf)
                    } else {
                        rsp.handle_request(spdm_in, &mut out_buf, &mut rng)
                    } {
                        Ok(n) => n,
                        Err(_) => {
                            // Emit a valid SPDM ERROR response instead of an empty DOE object.
                            fstart_spdm::write_error(
                                fstart_spdm::SPDM_VERSION_1_2,
                                fstart_spdm::SpdmErrorCode::Unspecified,
                                0,
                                &mut out_buf,
                            )
                            .unwrap_or(4)
                        }
                    };
                    let dws = 2 + rlen.div_ceil(4);
                    let mut doe_resp = vec![0u8; dws * 4];
                    doe_resp[0..4].copy_from_slice(&payload[0..4]);
                    doe_resp[4..8].copy_from_slice(&(dws as u32).to_le_bytes());
                    doe_resp[8..8 + rlen].copy_from_slice(&out_buf[..rlen]);
                    send_emu_frame(
                        &mut stream,
                        SOCKET_SPDM_COMMAND_NORMAL,
                        transport_type,
                        &doe_resp,
                    )?;
                } else {
                    let rlen = match rsp.handle_request(&payload, &mut out_buf, &mut rng) {
                        Ok(n) => n,
                        Err(_) => fstart_spdm::write_error(
                            fstart_spdm::SPDM_VERSION_1_2,
                            fstart_spdm::SpdmErrorCode::Unspecified,
                            0,
                            &mut out_buf,
                        )
                        .unwrap_or(4),
                    };
                    send_emu_frame(
                        &mut stream,
                        SOCKET_SPDM_COMMAND_NORMAL,
                        transport_type,
                        &out_buf[..rlen],
                    )?;
                }
            }
            _ => {
                send_emu_frame(&mut stream, cmd, transport_type, &[])?;
            }
        }
    }
}

fn send_emu_frame(
    stream: &mut TcpStream,
    cmd: u32,
    transport_type: u32,
    payload: &[u8],
) -> io::Result<()> {
    let hdr = SpdmEmuHeader {
        cmd: BeU32::new(cmd),
        transport_type: BeU32::new(transport_type),
        size: BeU32::new(payload.len() as u32),
    };
    stream.write_all(hdr.as_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

fn run_spdm_emu(port: u16) -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    eprintln!("fstart-mctp-broker: spdm-emu DOE server listening on 127.0.0.1:{port}");
    for incoming in listener.incoming() {
        let stream = incoming?;
        thread::spawn(move || {
            if let Err(e) = handle_spdm_emu_client(stream) {
                eprintln!("spdm-emu client error: {e}");
            }
        });
    }
    Ok(())
}

fn run_bridge(
    bmc_sock: PathBuf,
    host_sock: PathBuf,
    transport: TransportKind,
    pcap_path: Option<PathBuf>,
) -> io::Result<()> {
    // Refuse to overwrite existing files/sockets to avoid accidental deletion.
    if bmc_sock.exists() || host_sock.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "socket path already exists; remove stale socket explicitly before starting bridge",
        ));
    }
    let bmc_listener = UnixListener::bind(&bmc_sock)?;
    let host_listener = UnixListener::bind(&host_sock)?;
    let pcap = match pcap_path {
        Some(p) => Some(Arc::new(Mutex::new(PcapNgWriter::create(&p)?))),
        None => None,
    };

    eprintln!(
        "fstart-mctp-broker: waiting for BMC ({}) and Host ({})...",
        bmc_sock.display(),
        host_sock.display()
    );
    let (mut bmc_a, _) = bmc_listener.accept()?;
    let (mut host_a, _) = host_listener.accept()?;
    let mut bmc_b = bmc_a.try_clone()?;
    let mut host_b = host_a.try_clone()?;

    let pcap1 = pcap.clone();
    let t1 = thread::spawn(move || forward_stream(&mut bmc_a, &mut host_a, transport, pcap1));
    let t2 = thread::spawn(move || forward_stream(&mut host_b, &mut bmc_b, transport, pcap));
    let r1 = t1
        .join()
        .map_err(|_| io::Error::other("bmc->host worker panicked"))?;
    let r2 = t2
        .join()
        .map_err(|_| io::Error::other("host->bmc worker panicked"))?;
    r1.and(r2)
}

fn forward_stream(
    src: &mut UnixStream,
    dst: &mut UnixStream,
    transport: TransportKind,
    pcap: Option<Arc<Mutex<PcapNgWriter>>>,
) -> io::Result<()> {
    let mut buf = [0u8; 1024];
    let mut serial_dec = serial::SerialDecoder::new();
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            let _ = dst.shutdown(Shutdown::Write);
            return Ok(());
        }
        dst.write_all(&buf[..n])?;
        dst.flush()?;
        if let Some(ref writer) = pcap
            && transport == TransportKind::Serial
        {
            for &b in &buf[..n] {
                if let Ok(Some(pkt)) = serial_dec.push_byte(b) {
                    let mut w = writer
                        .lock()
                        .map_err(|_| io::Error::other("pcap writer lock poisoned"))?;
                    w.write_mctp_packet(pkt)?;
                }
            }
        }
    }
}

/// Socket-backed transport used by `self-test` to validate all 5 bindings with
/// real Unix sockets + Wireshark pcapng capture.
struct SocketTransport {
    kind: TransportKind,
    stream: UnixStream,
    pcap: Arc<Mutex<PcapNgWriter>>,
}

impl PacketTransport for SocketTransport {
    fn mtu(&self) -> usize {
        BASELINE_MTU
    }

    fn send_packet(&mut self, pkt: &[u8]) -> Result<(), MctpError> {
        if let Ok(mut w) = self.pcap.lock() {
            let _ = w.write_mctp_packet(pkt);
        }
        let mut wire = [0u8; 512];
        let n = match self.kind {
            TransportKind::Serial => serial::encode(pkt, &mut wire)?,
            TransportKind::Smbus => smbus::encode(0x30, 0x10, pkt, &mut wire)?,
            TransportKind::I3c => i3c::encode(0x45, false, pkt, &mut wire)?,
            TransportKind::Kcs => kcs::encode(pkt, &mut wire)?,
            TransportKind::PcieVdm => pcie_vdm::encode(
                pcie_vdm::PcieRouting::RouteById,
                0x0010,
                0x0020,
                pkt,
                &mut wire,
            )?,
        };
        let len_hdr = (n as u16).to_le_bytes();
        self.stream
            .write_all(&len_hdr)
            .and_then(|()| self.stream.write_all(&wire[..n]))
            .and_then(|()| self.stream.flush())
            .map_err(|_| MctpError::Transport)
    }

    fn recv_packet(&mut self, out: &mut [u8]) -> Result<usize, MctpError> {
        let mut len_hdr = [0u8; 2];
        self.stream
            .read_exact(&mut len_hdr)
            .map_err(|_| MctpError::Transport)?;
        let n = u16::from_le_bytes(len_hdr) as usize;
        let mut wire = vec![0u8; n];
        self.stream
            .read_exact(&mut wire)
            .map_err(|_| MctpError::Transport)?;
        match self.kind {
            TransportKind::Serial => serial::decode(&wire, out),
            TransportKind::Smbus => {
                let (_, pkt) = smbus::decode(&wire)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
            TransportKind::I3c => {
                let pkt = i3c::decode(0x45, false, &wire)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
            TransportKind::Kcs => {
                let pkt = kcs::decode(&wire)?.ok_or(MctpError::InvalidFrame)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
            TransportKind::PcieVdm => {
                let (_, pkt) = pcie_vdm::decode(&wire)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
        }
    }
}

fn run_self_test(pcap_path: PathBuf) -> io::Result<()> {
    let pcap = Arc::new(Mutex::new(PcapNgWriter::create(&pcap_path)?));
    for kind in [
        TransportKind::Serial,
        TransportKind::Smbus,
        TransportKind::I3c,
        TransportKind::Kcs,
        TransportKind::PcieVdm,
    ] {
        let (s_bmc, s_host) = UnixStream::pair()?;
        let pcap_bmc = pcap.clone();
        let pcap_host = pcap.clone();

        let keypair = SpdmKeyPair::from_secret_bytes(&[0x55; 32]).unwrap();
        let pub_xy = *keypair.public_xy();

        // Host thread (MCTP endpoint + PLDM responder + SPDM responder)
        let host_thread = thread::spawn(move || {
            let t = SocketTransport {
                kind,
                stream: s_host,
                pcap: pcap_host,
            };
            let mut ep = Endpoint::<_, 1024, 2>::new(
                t,
                ControlResponder::new(0x00, [0xAA; 16], &[0x01, 0x05, 0x06]),
            );
            let mut spdm =
                SpdmResponder::<512, 2>::new(SessionTransport::Mctp, keypair, b"host-cert")
                    .unwrap();
            let mut pldm = PldmResponder::<4, 2, 512>::new(0x21, 0x9E9E);
            pldm.platform
                .add_sensor(NumericSensor {
                    sensor_id: 1,
                    reading: 61_200,
                })
                .unwrap();
            let mut rng = DeterministicRng::new(0x9999);
            let mut rx = [0u8; 1024];
            let mut resp = [0u8; 1024];

            // Serve 5 plaintext SPDM requests (VCA + DIGESTS + KEY_EXCHANGE)
            // Control SetEID/GetEID are automatically handled inside recv_handling_control!
            for _ in 0..5 {
                let m = ep.recv_handling_control(&mut rx).unwrap();
                assert_eq!(m.msg_type, MessageType::Spdm as u8);
                let rlen = spdm.handle_request(m.payload, &mut resp, &mut rng).unwrap();
                ep.send_message(
                    m.src_eid,
                    MessageType::Spdm as u8,
                    Some(m.msg_tag),
                    &resp[..rlen],
                )
                .unwrap();
            }
            // Serve encrypted FINISH
            let m = ep.recv_handling_control(&mut rx).unwrap();
            let rlen = spdm.handle_secured_spdm(m.payload, &mut resp).unwrap();
            ep.send_message(
                m.src_eid,
                MessageType::SecuredMsg as u8,
                Some(m.msg_tag),
                &resp[..rlen],
            )
            .unwrap();

            // Serve 2 PLDM requests over SPDM-Secured MCTP
            {
                let mut sec = SecuredMctpChannel {
                    inner: &mut ep,
                    session: spdm.session.as_mut().unwrap(),
                };
                for _ in 0..2 {
                    let m = sec.recv_message(&mut rx).unwrap();
                    assert_eq!(m.msg_type, MessageType::Pldm as u8);
                    let rlen = pldm.handle_request(m.payload, &mut resp).unwrap();
                    sec.send_message(
                        m.src_eid,
                        MessageType::Pldm as u8,
                        Some(m.msg_tag),
                        &resp[..rlen],
                    )
                    .unwrap();
                }
            }
            // Serve encrypted END_SESSION
            let m = ep.recv_handling_control(&mut rx).unwrap();
            let rlen = spdm.handle_secured_spdm(m.payload, &mut resp).unwrap();
            ep.send_message(
                m.src_eid,
                MessageType::SecuredMsg as u8,
                Some(m.msg_tag),
                &resp[..rlen],
            )
            .unwrap();
        });

        // BMC side (Bus Owner + SPDM Requester + PLDM Client)
        let t = SocketTransport {
            kind,
            stream: s_bmc,
            pcap: pcap_bmc,
        };
        let mut bmc = Endpoint::<_, 1024, 2>::new(
            t,
            ControlResponder::new(0x08, [0xBB; 16], &[0x01, 0x05, 0x06]).with_bus_owner(true),
        );
        let mut spdm_req = SpdmRequester::<512>::new(SessionTransport::Mctp, pub_xy);
        let mut rng = DeterministicRng::new(0x7777);
        let mut tx = [0u8; 512];
        let mut rx = [0u8; 1024];

        // 1. MCTP Control: SetEID(0x0A) + GetEID
        let n = encode_set_eid_req(1, SetEidOperation::SetEid, 0x0A, &mut tx).unwrap();
        bmc.send_message(0x00, MessageType::Control as u8, None, &tx[..n])
            .unwrap();
        let m = bmc.recv_message(&mut rx).unwrap();
        assert_eq!(m.payload[4], 0x0A);

        let n = encode_get_eid_req(2, &mut tx).unwrap();
        bmc.send_message(0x0A, MessageType::Control as u8, None, &tx[..n])
            .unwrap();
        let m = bmc.recv_message(&mut rx).unwrap();
        assert_eq!(m.payload[3], 0x0A);

        // 2. SPDM handshake
        let n = spdm_req.build_get_version(&mut tx).unwrap();
        bmc.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
            .unwrap();
        spdm_req
            .handle_version(bmc.recv_message(&mut rx).unwrap().payload)
            .unwrap();

        let n = spdm_req.build_get_capabilities(&mut tx).unwrap();
        bmc.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
            .unwrap();
        spdm_req
            .handle_capabilities(bmc.recv_message(&mut rx).unwrap().payload)
            .unwrap();

        let n = spdm_req.build_negotiate_algorithms(&mut tx).unwrap();
        bmc.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
            .unwrap();
        spdm_req
            .handle_algorithms(bmc.recv_message(&mut rx).unwrap().payload)
            .unwrap();

        let n = spdm_req.build_get_digests(&mut tx).unwrap();
        bmc.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
            .unwrap();
        spdm_req
            .handle_digests(bmc.recv_message(&mut rx).unwrap().payload)
            .unwrap();

        let n = spdm_req.build_key_exchange(&mut rng, &mut tx).unwrap();
        bmc.send_message(0x0A, MessageType::Spdm as u8, None, &tx[..n])
            .unwrap();
        spdm_req
            .handle_key_exchange_rsp(&tx[..n], bmc.recv_message(&mut rx).unwrap().payload)
            .unwrap();

        let n = spdm_req.build_secured_finish(&mut tx).unwrap();
        bmc.send_message(0x0A, MessageType::SecuredMsg as u8, None, &tx[..n])
            .unwrap();
        spdm_req
            .handle_secured_finish_rsp(bmc.recv_message(&mut rx).unwrap().payload)
            .unwrap();

        // 3. PLDM over SPDM-Secured MCTP
        {
            let mut sec = SecuredMctpChannel {
                inner: &mut bmc,
                session: spdm_req.session.as_mut().unwrap(),
            };
            let n = encode_get_tid_req(1, &mut tx).unwrap();
            sec.send_message(0x0A, MessageType::Pldm as u8, None, &tx[..n])
                .unwrap();
            assert_eq!(sec.recv_message(&mut rx).unwrap().payload[4], 0x21);

            let n = encode_get_sensor_reading_req(2, 1, &mut tx).unwrap();
            sec.send_message(0x0A, MessageType::Pldm as u8, None, &tx[..n])
                .unwrap();
            let reply = sec.recv_message(&mut rx).unwrap();
            assert_eq!(
                u32::from_le_bytes(reply.payload[10..14].try_into().unwrap()),
                61_200
            );
        }

        // 4. END_SESSION
        let n = spdm_req
            .build_secured_cmd(SpdmCode::EndSession, 0, 0, &mut tx)
            .unwrap();
        bmc.send_message(0x0A, MessageType::SecuredMsg as u8, None, &tx[..n])
            .unwrap();
        spdm_req
            .handle_secured_ack(
                SpdmCode::EndSessionAck,
                bmc.recv_message(&mut rx).unwrap().payload,
            )
            .unwrap();

        host_thread.join().expect("host thread panicked");
        eprintln!("PASS: {kind:?} (MCTP Control + SPDM 1.2 + PLDM-over-Secured-MCTP)");
    }
    eprintln!(
        "Wrote Wireshark SLL2 ETH_P_MCTP pcapng to {}",
        pcap_path.display()
    );
    Ok(())
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Bridge {
            bmc_sock,
            host_sock,
            transport,
            pcap,
        } => run_bridge(bmc_sock, host_sock, transport.into(), pcap),
        Command::SpdmEmu { port } => run_spdm_emu(port),
        Command::SelfTest { pcap } => run_self_test(pcap),
        Command::QemuValidate {
            q35_bios,
            bmc_flash,
            pcap,
        } => qemu_validate::run(q35_bios, bmc_flash, pcap),
    }
}
