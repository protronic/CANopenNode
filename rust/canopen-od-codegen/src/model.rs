//! Data model of a CANopenEditor device description.
//!
//! Mirrors the `LibCanOpen` protobuf schema
//! (`CANopenEditor/libEDSsharp/proto/CanOpen.proto`) in its proto3 JSON
//! mapping (camelCase field names, enums as strings), as produced by
//! CANopenEditor's JSON export. Parsed with serde instead of protoc-generated
//! code to keep the build toolchain pure Rust; unknown fields are ignored,
//! absent fields default, so the model tolerates schema additions.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Root message: one device description.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanOpenDevice {
    #[serde(default)]
    pub file_info: FileInfo,
    #[serde(default)]
    pub device_info: DeviceInfo,
    /// Object dictionary, keyed by 4-digit uppercase hex index ("1017").
    #[serde(default)]
    pub objects: BTreeMap<String, OdObject>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub modification_time: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    #[serde(default)]
    pub vendor_name: String,
    #[serde(default)]
    pub product_name: String,
}

/// One OD entry (VAR, ARRAY or RECORD).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OdObject {
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub alias: String,
    /// "OBJECT_TYPE_VAR" | "OBJECT_TYPE_ARRAY" | "OBJECT_TYPE_RECORD".
    #[serde(default)]
    pub object_type: String,
    /// Label for the OD_CNT_* style object counts ("NMT", "TPDO", ...).
    #[serde(default)]
    pub count_label: String,
    /// Storage group ("" = RAM, "PERSIST_COMM", ...).
    #[serde(default)]
    pub storage_group: String,
    /// Sub-objects, keyed by 2-digit uppercase hex sub-index ("00").
    #[serde(default)]
    pub sub_objects: BTreeMap<String, OdSubObject>,
}

/// One sub-object.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OdSubObject {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub alias: String,
    /// "UNSIGNED16", "VISIBLE_STRING", "DOMAIN", ...
    #[serde(default)]
    pub data_type: String,
    /// "ACCESS_SDO_NO" | "..._RO" | "..._WO" | "..._RW".
    #[serde(default)]
    pub sdo: String,
    /// "ACCESS_PDO_NO" | "..._R" | "..._T" | "..._TR".
    #[serde(default)]
    pub pdo: String,
    #[serde(default)]
    pub default_value: String,
    /// Device-specific value; overrides `default_value` when non-empty.
    #[serde(default)]
    pub actual_value: String,
    #[serde(default)]
    pub low_limit: String,
    #[serde(default)]
    pub high_limit: String,
    #[serde(default)]
    pub string_length_min: u32,
}

impl OdObject {
    /// Preferred identifier source: alias if set, else name.
    pub fn ident_source(&self) -> &str {
        if self.alias.is_empty() {
            &self.name
        } else {
            &self.alias
        }
    }
}

impl OdSubObject {
    /// Preferred identifier source: alias if set, else name.
    pub fn ident_source(&self) -> &str {
        if self.alias.is_empty() {
            &self.name
        } else {
            &self.alias
        }
    }

    /// The value the device shall be initialized with.
    pub fn init_value(&self) -> &str {
        if self.actual_value.trim().is_empty() {
            &self.default_value
        } else {
            &self.actual_value
        }
    }
}
