//! Generator tests against a small synthetic device covering the code paths
//! the DS301 example profile does not exercise (strings, DOMAIN, value
//! limits, signed types, disabled objects).

const DEVICE: &str = r#"{
  "deviceInfo": { "productName": "Synthetic" },
  "objects": {
    "2000": {
      "name": "Device label",
      "objectType": "OBJECT_TYPE_VAR",
      "subObjects": {
        "00": { "name": "", "dataType": "VISIBLE_STRING", "sdo": "ACCESS_SDO_RW",
                 "defaultValue": "abc", "stringLengthMin": 12 }
      }
    },
    "2001": {
      "name": "Firmware image",
      "objectType": "OBJECT_TYPE_VAR",
      "subObjects": {
        "00": { "name": "", "dataType": "DOMAIN", "sdo": "ACCESS_SDO_WO" }
      }
    },
    "2002": {
      "name": "Temperature limit",
      "objectType": "OBJECT_TYPE_VAR",
      "subObjects": {
        "00": { "name": "", "dataType": "INTEGER16", "sdo": "ACCESS_SDO_RW",
                 "defaultValue": "-40", "lowLimit": "-55", "highLimit": "125" }
      }
    },
    "2003": {
      "name": "Old object",
      "disabled": true,
      "objectType": "OBJECT_TYPE_VAR",
      "subObjects": {
        "00": { "name": "", "dataType": "UNSIGNED8", "sdo": "ACCESS_SDO_RW" }
      }
    },
    "2004": {
      "name": "Actual wins",
      "objectType": "OBJECT_TYPE_VAR",
      "subObjects": {
        "00": { "name": "", "dataType": "UNSIGNED16", "sdo": "ACCESS_SDO_RO",
                 "defaultValue": "1", "actualValue": "0x2A" }
      }
    }
  }
}"#;

#[test]
fn synthetic_device_code_paths() {
    let code = canopen_od_codegen::generate(DEVICE).unwrap();

    // String: capacity honours stringLengthMin, default preserved.
    assert!(code.contains("pub x2000_device_label: od::OdString<12>"), "{code}");
    assert!(code.contains("od::OdString::new(b\"abc\")"));
    assert!(code.contains("(0x2000, 0x00) => self.x2000_device_label.set(data),"));

    // DOMAIN: no field, NoData on access.
    assert!(!code.contains("x2001_firmware_image:"));
    assert!(code.contains("(0x2001, 0x00) => Err(OdError::NoData),"));

    // Signed type with limits: range checks in the write arm.
    assert!(code.contains("x2002_temperature_limit: i16"));
    assert!(code.contains("if v < -55i16 { return Err(OdError::ValueTooLow); }"));
    assert!(code.contains("if v > 125i16 { return Err(OdError::ValueTooHigh); }"));

    // Disabled object is completely absent.
    assert!(!code.contains("0x2003"));

    // actualValue overrides defaultValue; read-only write arm.
    assert!(code.contains("x2004_actual_wins: 0x2Au16"));
    assert!(code.contains("(0x2004, 0x00) => Err(OdError::ReadOnly),"));
}

#[test]
fn rejects_unsupported_data_type() {
    let device = r#"{ "objects": { "2000": {
        "name": "x", "objectType": "OBJECT_TYPE_VAR",
        "subObjects": { "00": { "dataType": "REAL32", "sdo": "ACCESS_SDO_RO" } }
    } } }"#;
    let err = canopen_od_codegen::generate(device).unwrap_err();
    assert!(err.contains("unsupported data type"), "{err}");
    assert!(err.contains("0x2000"), "{err}");
}
