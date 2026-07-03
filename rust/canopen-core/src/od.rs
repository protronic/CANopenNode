//! Object dictionary interface, port of `301/CO_ODinterface.*`.
//!
//! The OD itself is generated at build time from a CANopenEditor protobuf
//! JSON device description (`.codev.json` / exported `.json`) by the
//! `canopen-od-codegen` crate; the generated struct implements
//! [`ObjectDictionary`]. Unlike the C stack there are no lock macros: the OD
//! is owned by the node task, and other tasks interact with it via messages.
//!
//! Values cross this interface as little-endian bytes, matching the SDO wire
//! format (CiA 301 §7.4.7).

use crate::sdo::SdoAbortCode;

/// CANopen static data types (CiA 301 §7.4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    /// BOOLEAN (1 byte on the wire).
    Boolean,
    /// INTEGER8.
    Integer8,
    /// INTEGER16.
    Integer16,
    /// INTEGER32.
    Integer32,
    /// INTEGER64.
    Integer64,
    /// UNSIGNED8.
    Unsigned8,
    /// UNSIGNED16.
    Unsigned16,
    /// UNSIGNED32.
    Unsigned32,
    /// UNSIGNED64.
    Unsigned64,
    /// REAL32.
    Real32,
    /// REAL64.
    Real64,
    /// VISIBLE_STRING.
    VisibleString,
    /// OCTET_STRING.
    OctetString,
    /// UNICODE_STRING.
    UnicodeString,
    /// DOMAIN (application-defined data, no OD-side storage).
    Domain,
}

/// SDO access rights of a sub-object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdoAccess {
    /// No SDO access.
    No,
    /// Read-only.
    ReadOnly,
    /// Write-only.
    WriteOnly,
    /// Read and write.
    ReadWrite,
}

impl SdoAccess {
    /// Whether SDO reads are permitted.
    pub const fn readable(self) -> bool {
        matches!(self, Self::ReadOnly | Self::ReadWrite)
    }

    /// Whether SDO writes are permitted.
    pub const fn writable(self) -> bool {
        matches!(self, Self::WriteOnly | Self::ReadWrite)
    }
}

/// PDO mapping capability of a sub-object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdoAccess {
    /// Not mappable.
    No,
    /// Mappable into RPDOs (received, written by the stack).
    Rpdo,
    /// Mappable into TPDOs (transmitted, read by the stack).
    Tpdo,
    /// Mappable in both directions.
    Both,
}

/// Errors of OD access, subset of `ODR_t` with the same SDO abort mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OdError {
    /// Object does not exist (`ODR_IDX_NOT_EXIST`).
    ObjectNotFound,
    /// Sub-index does not exist (`ODR_SUB_NOT_EXIST`).
    SubIndexNotFound,
    /// Read attempted on a write-only entry (`ODR_WRITEONLY`).
    WriteOnly,
    /// Write attempted on a read-only entry (`ODR_READONLY`).
    ReadOnly,
    /// Data length does not match the entry (`ODR_TYPE_MISMATCH`).
    TypeMismatch,
    /// Data too long for the entry (`ODR_DATA_LONG`).
    DataTooLong,
    /// Written value above the entry's high limit (`ODR_VALUE_HIGH`).
    ValueTooHigh,
    /// Written value below the entry's low limit (`ODR_VALUE_LOW`).
    ValueTooLow,
    /// No data available, e.g. DOMAIN without an application handler
    /// (`ODR_NO_DATA`).
    NoData,
    /// Caller's read buffer is too small for the value.
    BufferTooSmall,
}

impl OdError {
    /// The SDO abort code an SDO server must answer with for this error.
    pub fn abort_code(self) -> SdoAbortCode {
        match self {
            Self::ObjectNotFound => SdoAbortCode::NO_OBJECT,
            Self::SubIndexNotFound => SdoAbortCode::SUB_UNKNOWN,
            Self::WriteOnly => SdoAbortCode::WRITE_ONLY,
            Self::ReadOnly => SdoAbortCode::READ_ONLY,
            Self::TypeMismatch => SdoAbortCode::TYPE_MISMATCH,
            Self::DataTooLong => SdoAbortCode::DATA_LONG,
            Self::ValueTooHigh => SdoAbortCode::VALUE_HIGH,
            Self::ValueTooLow => SdoAbortCode::VALUE_LOW,
            Self::NoData => SdoAbortCode::NO_DATA,
            Self::BufferTooSmall => SdoAbortCode::GENERAL,
        }
    }
}

/// Metadata of one sub-object, the counterpart of `OD_getSub()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryInfo {
    /// Static data type of the entry.
    pub data_type: DataType,
    /// SDO access rights.
    pub sdo: SdoAccess,
    /// PDO mapping capability.
    pub pdo: PdoAccess,
    /// Current data size in bytes (actual length for strings, 0 for DOMAIN).
    pub size: usize,
}

/// Access to a node's object dictionary, the counterpart of the
/// `OD_find`/`OD_getSub`/read/write API in `301/CO_ODinterface.h`.
///
/// Implemented by generated OD structs (`canopen-od-codegen`). Consumed by
/// the SDO server, PDO mapping and the application.
pub trait ObjectDictionary {
    /// Look up entry metadata.
    fn info(&self, index: u16, sub: u8) -> Result<EntryInfo, OdError>;

    /// Read the value of `index:sub` as little-endian bytes into `buf`,
    /// returning the number of bytes. Enforces SDO read access.
    fn read(&self, index: u16, sub: u8, buf: &mut [u8]) -> Result<usize, OdError>;

    /// Write little-endian bytes to `index:sub`, enforcing SDO write access,
    /// exact data length and the entry's value limits.
    fn write(&mut self, index: u16, sub: u8, data: &[u8]) -> Result<(), OdError>;
}

/// Fixed-capacity byte string backing VISIBLE_STRING / OCTET_STRING entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OdString<const N: usize> {
    len: u16,
    bytes: [u8; N],
}

impl<const N: usize> OdString<N> {
    /// Create from initial content; `init` longer than `N` is truncated
    /// (the code generator sizes `N` to fit the configured default).
    pub const fn new(init: &[u8]) -> Self {
        let len = if init.len() > N { N } else { init.len() };
        let mut bytes = [0u8; N];
        let mut i = 0;
        while i < len {
            bytes[i] = init[i];
            i += 1;
        }
        Self {
            len: len as u16,
            bytes,
        }
    }

    /// Current content.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// Current length in bytes.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the string is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Replace the content; fails with [`OdError::DataTooLong`] if `data`
    /// exceeds the capacity.
    pub fn set(&mut self, data: &[u8]) -> Result<(), OdError> {
        if data.len() > N {
            return Err(OdError::DataTooLong);
        }
        self.bytes[..data.len()].copy_from_slice(data);
        self.len = data.len() as u16;
        Ok(())
    }
}

/// Copy a value into a read buffer (helper for generated code).
pub fn read_bytes(buf: &mut [u8], value: &[u8]) -> Result<usize, OdError> {
    if buf.len() < value.len() {
        return Err(OdError::BufferTooSmall);
    }
    buf[..value.len()].copy_from_slice(value);
    Ok(value.len())
}

/// Require an exact data length (helper for generated code); SDO expedited
/// writes carry the exact object size per CiA 301.
pub fn exact<const N: usize>(data: &[u8]) -> Result<[u8; N], OdError> {
    data.try_into().map_err(|_| OdError::TypeMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn od_string_semantics() {
        let mut s: OdString<8> = OdString::new(b"abc");
        assert_eq!(s.as_bytes(), b"abc");
        assert_eq!(s.len(), 3);
        s.set(b"defgh").unwrap();
        assert_eq!(s.as_bytes(), b"defgh");
        assert_eq!(s.set(b"123456789"), Err(OdError::DataTooLong));
        // Truncating construction.
        let t: OdString<2> = OdString::new(b"xyz");
        assert_eq!(t.as_bytes(), b"xy");
    }

    #[test]
    fn helpers() {
        let mut buf = [0u8; 4];
        assert_eq!(read_bytes(&mut buf, &[1, 2]), Ok(2));
        assert_eq!(&buf[..2], &[1, 2]);
        let mut small = [0u8; 1];
        assert_eq!(read_bytes(&mut small, &[1, 2]), Err(OdError::BufferTooSmall));

        assert_eq!(exact::<2>(&[1, 2]), Ok([1, 2]));
        assert_eq!(exact::<2>(&[1]), Err(OdError::TypeMismatch));
    }

    #[test]
    fn abort_mapping() {
        assert_eq!(
            OdError::ObjectNotFound.abort_code(),
            SdoAbortCode::NO_OBJECT
        );
        assert_eq!(OdError::ReadOnly.abort_code(), SdoAbortCode::READ_ONLY);
        assert_eq!(OdError::NoData.abort_code(), SdoAbortCode::NO_DATA);
    }
}
