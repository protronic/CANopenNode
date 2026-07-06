//! Behavioral tests of the OD generated from `example/DS301_profile.json`,
//! exercising the `ObjectDictionary` impl the way the SDO server will.

use canopen_core::od::{DataType, ObjectDictionary, OdError, SdoAccess};
use canopen_core::sdo::SdoAbortCode;
use canopen_core::NodeId;
use canopen_example_od::{Od, CNT_RPDO, CNT_TPDO};

fn od() -> Od {
    Od::new(NodeId::new(0x0A).unwrap())
}

fn read_u32(od: &Od, index: u16, sub: u8) -> u32 {
    let mut buf = [0u8; 4];
    assert_eq!(od.read(index, sub, &mut buf), Ok(4), "{index:#06X}:{sub:02X}");
    u32::from_le_bytes(buf)
}

#[test]
fn defaults_and_node_id_relative_cob_ids() {
    let od = od();
    // 0x1000 device type: plain default.
    assert_eq!(read_u32(&od, 0x1000, 0), 0);
    // Default SDO server channel COB-IDs are $NODEID-relative.
    assert_eq!(read_u32(&od, 0x1200, 1), 0x60A);
    assert_eq!(read_u32(&od, 0x1200, 2), 0x58A);
    // EMCY COB-ID = 0x80 + node id.
    assert_eq!(read_u32(&od, 0x1014, 0), 0x8A);
    // Direct typed field access for the application.
    assert_eq!(od.x1200_cob_id_client_to_server_rx, 0x60A);
}

#[test]
fn write_respects_access_rights() {
    let mut od = od();
    // 0x1017 heartbeat time is rw.
    od.write(0x1017, 0, &500u16.to_le_bytes()).unwrap();
    assert_eq!(od.x1017_producer_heartbeat_time, 500);
    // Wrong length is a type mismatch.
    assert_eq!(
        od.write(0x1017, 0, &500u32.to_le_bytes()),
        Err(OdError::TypeMismatch)
    );
    // 0x1000 device type is ro.
    assert_eq!(
        od.write(0x1000, 0, &1u32.to_le_bytes()),
        Err(OdError::ReadOnly)
    );
}

#[test]
fn missing_objects_and_subs() {
    let od = od();
    let mut buf = [0u8; 8];
    assert_eq!(od.read(0x5000, 0, &mut buf), Err(OdError::ObjectNotFound));
    assert_eq!(od.read(0x1000, 1, &mut buf), Err(OdError::SubIndexNotFound));
    assert_eq!(
        od.info(0x1000, 1).unwrap_err().abort_code(),
        SdoAbortCode::SUB_UNKNOWN
    );
}

#[test]
fn array_access() {
    let mut od = od();
    // 0x1003 pre-defined error field: count sub is rw, elements ro.
    let mut buf = [0u8; 4];
    assert_eq!(od.read(0x1003, 5, &mut buf), Ok(4));
    assert_eq!(
        od.write(0x1003, 5, &1u32.to_le_bytes()),
        Err(OdError::ReadOnly)
    );
    od.write(0x1003, 0, &[0u8]).unwrap();
    // The application writes elements directly.
    od.x1003_pre_defined_error_field[4] = 0xDEAD_BEEF;
    assert_eq!(read_u32(&od, 0x1003, 5), 0xDEAD_BEEF);
    // Element 17 does not exist (16 elements).
    assert_eq!(od.read(0x1003, 17, &mut buf), Err(OdError::SubIndexNotFound));
}

#[test]
fn entry_info_metadata() {
    let od = od();
    let info = od.info(0x1017, 0).unwrap();
    assert_eq!(info.data_type, DataType::Unsigned16);
    assert_eq!(info.sdo, SdoAccess::ReadWrite);
    assert_eq!(info.size, 2);

    // 0x1005 COB-ID SYNC message: u32, rw.
    let info = od.info(0x1005, 0).unwrap();
    assert_eq!(info.data_type, DataType::Unsigned32);
    assert_eq!(info.sdo, SdoAccess::ReadWrite);
    assert_eq!(info.size, 4);
}

#[test]
fn disabled_objects_are_not_generated() {
    // 0x1008 (device name) and 0x1021 (store EDS) exist in the profile but
    // are disabled — exactly like in the C example OD.h.
    let od = od();
    let mut buf = [0u8; 8];
    assert_eq!(od.read(0x1008, 0, &mut buf), Err(OdError::ObjectNotFound));
    assert_eq!(od.info(0x1021, 0), Err(OdError::ObjectNotFound));
}

#[test]
fn object_counts_match_c_od() {
    // Same values as OD_CNT_RPDO / OD_CNT_TPDO in the C example OD.h:
    // objects 0x1404/0x1804 exist in the profile but are disabled.
    assert_eq!(CNT_RPDO, 4);
    assert_eq!(CNT_TPDO, 4);
}
