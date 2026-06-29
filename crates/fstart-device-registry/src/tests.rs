use super::*;

#[test]
fn service_set_iterates_inserted_services() {
    let mut services = ServiceSet::empty();
    services.insert(Service::Console);
    services.insert(Service::BlockDevice);

    let collected: Vec<_> = services.iter().collect();
    assert_eq!(collected, vec![Service::Console, Service::BlockDevice]);
}

#[test]
fn service_all_covers_every_bit_position() {
    for service in Service::ALL {
        let mut services = ServiceSet::empty();
        services.insert(*service);
        assert_eq!(services.iter().count(), 1);
        assert_eq!(services.iter().next(), Some(*service));
    }
}

#[test]
fn structural_reports_structural_construction_kind() {
    let inst = DriverInstance::Structural(StructuralConfig::default());
    assert_eq!(inst.construction_kind(), ConstructionKind::Structural);
    assert!(!inst.has_runtime_driver());
    assert!(inst.provided_services().is_empty());
}

#[cfg(feature = "ns16550")]
#[test]
fn runtime_driver_reports_device_construction_kind() {
    let inst = DriverInstance::Ns16550(ns16550::Ns16550Config {
        regs: ns16550::AccessMode::Mmio {
            base: 0x1000_0000,
            reg_shift: 0,
            reg_width: 0,
        },
        clock_freq: 3_686_400,
        baud_rate: 115_200,
    });
    assert_eq!(inst.construction_kind(), ConstructionKind::Device);
    assert!(inst.has_runtime_driver());
    assert!(inst.provides(Service::Console));
}

#[cfg(feature = "ite8721f")]
#[test]
fn superio_console_service_depends_on_console_port() {
    let without_console = DriverInstance::Ite8721f(ite8721f::Ite8721fConfig::default());
    assert!(without_console.provides(Service::SuperIoHost));
    assert!(!without_console.provides(Service::Console));

    let mut cfg = ite8721f::Ite8721fConfig::default();
    cfg.console_port = Some(heapless::String::try_from("com1").unwrap());
    let with_console = DriverInstance::Ite8721f(cfg);
    assert!(with_console.provides(Service::Console));
}

#[cfg(feature = "nsc-pc87382")]
#[test]
fn pc87382_console_service_depends_on_console_port() {
    let without_console = DriverInstance::NscPc87382(nsc_pc87382::Pc87382Config::default());
    assert!(without_console.provides(Service::SuperIoHost));
    assert!(!without_console.provides(Service::Console));

    let mut cfg = nsc_pc87382::Pc87382Config::default();
    cfg.console_port = Some(heapless::String::try_from("com2").unwrap());
    let with_console = DriverInstance::NscPc87382(cfg);
    assert!(with_console.provides(Service::Console));
}

#[cfg(feature = "nsc-pc87392")]
#[test]
fn pc87392_console_service_depends_on_console_port() {
    let without_console = DriverInstance::NscPc87392(nsc_pc87392::Pc87392Config::default());
    assert!(without_console.provides(Service::SuperIoHost));
    assert!(!without_console.provides(Service::Console));

    let mut cfg = nsc_pc87392::Pc87392Config::default();
    cfg.console_port = Some(heapless::String::try_from("com1").unwrap());
    let with_console = DriverInstance::NscPc87392(cfg);
    assert!(with_console.provides(Service::Console));
}

#[cfg(feature = "intel-ich7")]
#[test]
fn ich7_reports_runtime_smbus_service() {
    let inst = DriverInstance::IntelIch7(intel_ich7::IntelIch7Config {
        rcba: 0xfed1_0000,
        pirq_routing: [0; 8],
        gpe0_en: 0,
        lpc_decode: Default::default(),
        hda: None,
        sata: None,
        usb: None,
        pata: false,
        smbus_base: 0x0400,
        gpio: Default::default(),
        acpi_name: None,
        c3_latency: 85,
        power_on_after_fail: 0,
    });
    assert!(inst.provides(Service::Southbridge));
    assert!(inst.provides(Service::SystemManagementBus));
}

#[cfg(feature = "intel-ich8")]
#[test]
fn ich8_reports_runtime_smbus_service() {
    let inst = DriverInstance::IntelIch8(intel_ich8::IntelIch8Config {
        rcba: 0xfed1_0000,
        dmibar: 0xfed1_8000,
        pirq_routing: [0; 8],
        gpe0_en: 0,
        gpi_routing: [0; 16],
        alt_gp_smi_en: 0,
        c4_on_c3: false,
        c5_enable: false,
        c6_enable: false,
        lpc_decode: Default::default(),
        hda: None,
        ide: None,
        sata: None,
        usb: None,
        pcie_ports: [true; 6],
        pcie_slots: [false; 6],
        pcie_power_limits: [Default::default(); 6],
        io_traps: Default::default(),
        smbus_base: 0x0400,
        gpio: Default::default(),
        acpi_name: None,
        c3_latency: 85,
        power_on_after_fail: 0,
        throttle_duty: 0,
        disable_lan: false,
        disable_sata2: true,
        disable_thermal: true,
    });
    assert!(inst.provides(Service::Southbridge));
    assert!(inst.provides(Service::SystemManagementBus));
}
