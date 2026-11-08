// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs IOD PCIe transmit requester identity, separate from IOHC config
//! remapping. openSIL PcieCommonEngineConfiguration / PORT_SPACE: 1a340084h +
//! physical RBIndex*1MiB + WrapId*4MiB + wrapper-local PortId*4KiB. Never use
//! LBID or the logical bridge/devfn index as PortId. Siena's vendor firmware
//! leaves the same values.
//! SP3 policy is not implied.
use crate::{
    mpio::ports,
    smn::{SmnAccess, SmnReg},
};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};
register_bitfields![u32,
    pub TxRequester [FUNCTION OFFSET(0) NUMBITS(3) [],DEVICE OFFSET(3) NUMBITS(5) [],BUS OFFSET(8) NUMBITS(8) [],SWUS OFFSET(16) NUMBITS(16) []]
];
#[derive(Clone, Copy, Debug)]
pub struct Port {
    pub root: u8,
    pub wrapper: u8,
    pub local_port: u8,
    pub bus: u8,
    pub devfn: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum Error {
    Identity,
    Duplicate,
    Unavailable,
    Readback,
    Capacity,
}
impl Port {
    /// IOHC remap slot (not LBID) to the wrapper-local address coordinates.
    pub fn from_slot(root: u8, slot: u8, bus: u8, devfn: u8) -> Result<Self, Error> {
        let (wrapper, local_port) = match slot {
            0..=8 => (0, slot),
            9..=17 => (1, slot - 9),
            18..=21 => (2, slot - 18),
            _ => return Err(Error::Identity),
        };
        let port = Self {
            root,
            wrapper,
            local_port,
            bus,
            devfn,
        };
        port.register()?;
        Ok(port)
    }
    pub fn register(self) -> Result<SmnReg<TxRequester::Register>, Error> {
        ports::slot(self.root, self.wrapper, self.local_port).map_err(|_| Error::Identity)?;
        if !ports::NATIVE_DEVFNS[..22].contains(&self.devfn) {
            return Err(Error::Identity);
        }
        Ok(SmnReg::new(
            0x1a340084
                + (u32::from(self.root) << 20)
                + (u32::from(self.wrapper) << 22)
                + (u32::from(self.local_port) << 12),
        ))
    }
}
/// Program live, stable, allocated port identities after training. Preflight
/// every target before the first write and preserve the independent SWUS ID.
/// # Safety
/// Checked Rs IOD, BSP-exclusive SMN/config, ports bound to actual IOHC map
/// and PCI root buses, interrupts masked and DMA disabled. A readback failure
/// after any write is fail-stop until reset, never retry or hand off.
pub unsafe fn initialize_requesters(smn: &impl SmnAccess, ports: &[Port]) -> Result<(), Error> {
    let mut updates = heapless::Vec::<(SmnReg<TxRequester::Register>, u32), 32>::new();
    for (i, port) in ports.iter().enumerate() {
        let reg = port.register()?;
        if ports[..i].iter().any(|p| {
            p.register()
                .is_ok_and(|previous| previous.address() == reg.address())
                || (p.root == port.root && (p.bus != port.bus || p.devfn == port.devfn))
                || (p.root != port.root && p.bus == port.bus)
        }) {
            return Err(Error::Duplicate);
        }
        let old = smn.reg(reg).get();
        if old == u32::MAX {
            return Err(Error::Unavailable);
        }
        let fields = TxRequester::FUNCTION.val(u32::from(port.devfn & 7))
            + TxRequester::DEVICE.val(u32::from(port.devfn >> 3))
            + TxRequester::BUS.val(u32::from(port.bus));
        let expected = fields.modify(old);
        updates.push((reg, expected)).map_err(|_| Error::Capacity)?;
    }
    for (reg, expected) in updates {
        smn.reg(reg).set(expected);
        if smn.reg(reg).get() != expected {
            return Err(Error::Readback);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;
    #[test]
    fn requester_address_uses_physical_root_wrapper_and_local_port_not_lbid() {
        let bmc = Port {
            root: 0,
            wrapper: 2,
            local_port: 1,
            bus: 0xc0,
            devfn: 0x2a,
        };
        assert_eq!(bmc.register().unwrap().address(), 0x1ab41084);
        let nvme = Port {
            root: 2,
            wrapper: 0,
            local_port: 0,
            bus: 0,
            devfn: 9,
        };
        assert_eq!(nvme.register().unwrap().address(), 0x1a540084);
        assert!(
            Port {
                local_port: 19,
                ..bmc
            }
            .register()
            .is_err()
        );
        assert!(Port { root: 1, ..bmc }.register().is_err());
    }
    #[test]
    fn all_targets_preflight_and_swus_identity_is_preserved() {
        let model = SmnSpace::default();
        let ports = [
            Port {
                root: 0,
                wrapper: 2,
                local_port: 1,
                bus: 0xc0,
                devfn: 0x2a,
            },
            Port {
                root: 0,
                wrapper: 2,
                local_port: 2,
                bus: 0xc0,
                devfn: 0x2b,
            },
            Port {
                root: 2,
                wrapper: 0,
                local_port: 0,
                bus: 0,
                devfn: 9,
            },
        ];
        for port in ports {
            model.set(port.register().unwrap(), 0xa55a0000);
        }
        model.set(ports[2].register().unwrap(), u32::MAX);
        assert_eq!(
            unsafe { initialize_requesters(&model.smn(), &ports) },
            Err(Error::Unavailable)
        );
        assert_eq!(model.get(ports[0].register().unwrap()), 0xa55a0000);
        model.set(ports[2].register().unwrap(), 0xbeef0000);
        unsafe { initialize_requesters(&model.smn(), &ports) }.unwrap();
        assert_eq!(model.get(ports[0].register().unwrap()), 0xa55ac02a);
        assert_eq!(model.get(ports[1].register().unwrap()), 0xa55ac02b);
        assert_eq!(model.get(ports[2].register().unwrap()), 0xbeef0009);
        assert_eq!(
            unsafe { initialize_requesters(&model.smn(), &[ports[0], ports[0]]) },
            Err(Error::Duplicate)
        );
    }
}
