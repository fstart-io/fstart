// SPDX-License-Identifier: MPL-2.0
// Legacy layouts: interoperability definitions, not copies of AMD code.
// References (pinned): openSIL/AGCL-R d30d3876c273575d4b00fd1bd5d4b9d4229ee101,
// AgesaPkg/Addendum/Apcb/Inc/Apcb.h (PSCFG_DATABUS_ENTRY_D4 and error output),
// Inc/CommonV3/ApcbMemSpdInfo.h (SPD descriptors). coreboot util/apcb/apcb_edit.py
// independently confirms the 28-byte descriptor and 512-byte SPD area.

#[derive(
    FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned, Debug, Copy, Clone,
)]
#[repr(transparent)]
pub struct SpdData(pub [u8; 512]);
impl Default for SpdData {
    fn default() -> Self {
        Self([0; 512])
    }
}
#[cfg(feature = "serde")]
impl serde::Serialize for SpdData {
    fn serialize<S: serde::Serializer>(
        &self,
        s: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        self.0.as_slice().serialize(s)
    }
}
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for SpdData {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> core::result::Result<Self, D::Error> {
        let data = Vec::<u8>::deserialize(d)?;
        let bytes: [u8; 512] = data.try_into().map_err(|v: Vec<u8>| {
            serde::de::Error::custom(format!(
                "SPD needs 512 bytes, got {}",
                v.len()
            ))
        })?;
        Ok(Self(bytes))
    }
}
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for SpdData {
    fn schema_name() -> String {
        "SpdData".into()
    }
    fn json_schema(
        g: &mut schemars::r#gen::SchemaGenerator,
    ) -> schemars::schema::Schema {
        let mut schema = <Vec<u8>>::json_schema(g).into_object();
        let array = schema.array.get_or_insert_with(Default::default);
        array.min_items = Some(512);
        array.max_items = Some(512);
        schema.into()
    }
}

make_accessors! {
    #[derive(FromBytes,IntoBytes,Immutable,KnownLayout,Unaligned,Debug,Copy,Clone)]
    #[repr(C,packed)]
    pub struct SpdDescriptor {
        dram_down_spd_valid || bool : BU8 | pub get bool : pub set bool,
        dimm_present || bool : BU8 | pub get bool : pub set bool,
        page_address:u8,
        nvdimm_present || bool : BU8 | pub get bool : pub set bool,
        dram_manufacturer_id || SerdeHex16:LU16,
        module_manufacturer_id || SerdeHex16:LU16,
        address || SerdeHex32:LU32,
        mux_present || bool:BU8,
        mux_address:u8,
        mux_channel:u8,
        _reserved_mux || #[serde(default)] u8:u8,
        technology || SerdeHex32:LU32,
        package || SerdeHex32:LU32,
        socket:u8,
        channel:u8 | pub get u8 : pub set u8,
        dimm:u8 | pub get u8 : pub set u8,
        shadow_spd_valid || bool:BU8,
    }
}
impl Default for SpdDescriptor {
    fn default() -> Self {
        <Self as zerocopy::FromZeros>::new_zeroed()
    }
}
impl crate::struct_accessors::Getter<Result<SpdDescriptor>> for SpdDescriptor {
    fn get1(self) -> Result<Self> {
        Ok(self)
    }
}
make_accessors! {
    #[derive(FromBytes,IntoBytes,Immutable,KnownLayout,Unaligned,Debug,Copy,Clone)]
    #[repr(C,packed)]
    pub struct SpdDimm {
        pub descriptor:SpdDescriptor,
        pub data || #[serde(default)] SpdData:SpdData,
    }
}
impl Default for SpdDimm {
    fn default() -> Self {
        <Self as zerocopy::FromZeros>::new_zeroed()
    }
}
impl crate::struct_accessors::Getter<Result<SpdData>> for SpdData {
    fn get1(self) -> Result<SpdData> {
        Ok(self)
    }
}
impl crate::struct_accessors::Setter<SpdData> for SpdData {
    fn set1(&mut self, v: SpdData) {
        *self = v
    }
}
make_accessors! {
    #[derive(FromBytes,IntoBytes,Immutable,KnownLayout,Unaligned,Debug,Copy,Clone,Default)]
    #[repr(C,packed)]
    pub struct SpdInfo {
        total_dimms || SerdeHex32:LU32,
    }
}
impl HeaderWithTail for SpdInfo {
    type TailArrayItemType<'de> = SpdDimm;
}
impl EntryCompatible for SpdInfo {
    fn is_entry_compatible(id: EntryId, body: &[u8]) -> bool {
        matches!(id, EntryId::Memory(MemoryEntryId::SpdInfo))
            && Self::read_from_prefix(body).is_ok_and(|(header, tail)| {
                tail.len().is_multiple_of(core::mem::size_of::<SpdDimm>())
                    && header.total_dimms.get() as usize
                        == tail.len() / core::mem::size_of::<SpdDimm>()
            })
    }
}

// A distinct 44-byte layout: no PMU PHY Vref / DRAM Vref tail exists here.
// Legacy voltage selectors retain the three-bit legacy voltage encoding.
macro_rules! legacy_data_bus {($name:ident,$ranks:ty,$($id:ident),+) => {
    make_accessors! {
        #[derive(FromBytes,IntoBytes,Immutable,KnownLayout,Unaligned,Debug,Copy,Clone)]
        #[repr(C,packed)]
        pub struct $name {
            dimm_slots_per_channel || SerdeHex32:LU32,
            ddr_rates || DdrRates:LU32,
            vdd_io || UdimmDdr4Voltages:LU32,
            dimm0_ranks || $ranks:LU32,
            dimm1_ranks || $ranks:LU32,
            rtt_nom || RttNom:LU32,
            rtt_wr || RttWr:LU32,
            rtt_park || RttPark:LU32,
            dq_drive_strength || SerdeHex32:LU32,
            dqs_drive_strength || SerdeHex32:LU32,
            odt_drive_strength || SerdeHex32:LU32,
        }
    }
    impl Default for $name {fn default()->Self{<Self as zerocopy::FromZeros>::new_zeroed()}}
    impl EntryCompatible for $name {
        fn is_entry_compatible(id:EntryId,_:&[u8])->bool {matches!(id,$(EntryId::Memory(MemoryEntryId::$id))|+)}
    }
};}
legacy_data_bus!(
    LegacyDdr4DataBusElement44,
    Ddr4DimmRanks,
    PsUdimmDdr4DataBus,
    PsRdimmDdr4DataBus
);
legacy_data_bus!(
    LegacyLrdimmDdr4DataBusElement44,
    LrdimmDdr4DimmRanks,
    PsLrdimmDdr4DataBus
);

make_accessors! {
    #[derive(FromBytes,IntoBytes,Immutable,KnownLayout,Unaligned,Debug,Copy,Clone)]
    #[repr(C,packed)]
    pub struct ErrorOutControl108 {
        enable_error_reporting || bool:BU8,
        enable_error_reporting_gpio || bool:BU8,
        enable_error_reporting_beep_codes || bool:BU8,
        enable_using_handshake || bool:BU8,
        input_port || SerdeHex32:LU32,
        output_delay || SerdeHex32:LU32,
        output_port || SerdeHex32:LU32,
        stop_on_first_fatal_error || bool:BU8,
        _reserved_stop || #[serde(default)] [SerdeHex8;3]:[u8;3],
        input_port_size || PortSize:LU32,
        output_port_size || PortSize:LU32,
        input_port_type || PortType:LU32,
        output_port_type || PortType:LU32,
        clear_acknowledgement || bool:BU8,
        error_reporting_gpio:Gpio,
        beep_code_table:[ErrorOutControlBeepCode;8],
        enable_heart_beat || bool:BU8,
        _reserved_end || #[serde(default)] [SerdeHex8;3]:[u8;3],
    }
}
impl Default for ErrorOutControl108 {
    fn default() -> Self {
        <Self as zerocopy::FromZeros>::new_zeroed()
    }
}
impl HeaderWithTail for ErrorOutControl108 {
    type TailArrayItemType<'de> = ();
}
impl EntryCompatible for ErrorOutControl108 {
    fn is_entry_compatible(id: EntryId, body: &[u8]) -> bool {
        matches!(id, EntryId::Memory(MemoryEntryId::ErrorOutControl))
            && body.len() == core::mem::size_of::<Self>()
    }
}

const _: () = assert!(core::mem::size_of::<SpdDescriptor>() == 28);
const _: () = assert!(core::mem::size_of::<SpdDimm>() == 540);
const _: () = assert!(core::mem::size_of::<SpdInfo>() == 4);
const _: () = assert!(core::mem::size_of::<LegacyDdr4DataBusElement44>() == 44);
const _: () =
    assert!(core::mem::size_of::<LegacyLrdimmDdr4DataBusElement44>() == 44);
const _: () = assert!(core::mem::size_of::<ErrorOutControl108>() == 108);
