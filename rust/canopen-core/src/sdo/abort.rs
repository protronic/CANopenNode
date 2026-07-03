//! SDO abort codes (CiA 301 §7.2.4.3.17), port of `CO_SDO_abortCode_t`.

/// An SDO abort code as carried in bytes 4..8 of an SDO abort frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdoAbortCode(pub u32);

impl SdoAbortCode {
    /// Toggle bit not alternated.
    pub const TOGGLE_BIT: Self = Self(0x0503_0000);
    /// SDO protocol timed out.
    pub const TIMEOUT: Self = Self(0x0504_0000);
    /// Client/server command specifier not valid or unknown.
    pub const CMD_SPECIFIER: Self = Self(0x0504_0001);
    /// Out of memory.
    pub const OUT_OF_MEMORY: Self = Self(0x0504_0005);
    /// Unsupported access to an object.
    pub const UNSUPPORTED_ACCESS: Self = Self(0x0601_0000);
    /// Attempt to read a write-only object.
    pub const WRITE_ONLY: Self = Self(0x0601_0001);
    /// Attempt to write a read-only object.
    pub const READ_ONLY: Self = Self(0x0601_0002);
    /// Object does not exist in the object dictionary.
    pub const NO_OBJECT: Self = Self(0x0602_0000);
    /// Object cannot be mapped to the PDO.
    pub const NO_MAP: Self = Self(0x0604_0041);
    /// Data type does not match, length of service parameter does not match.
    pub const TYPE_MISMATCH: Self = Self(0x0607_0010);
    /// Data type does not match, length of service parameter too high.
    pub const DATA_LONG: Self = Self(0x0607_0012);
    /// Data type does not match, length of service parameter too short.
    pub const DATA_SHORT: Self = Self(0x0607_0013);
    /// Sub-index does not exist.
    pub const SUB_UNKNOWN: Self = Self(0x0609_0011);
    /// Invalid value for parameter (download only).
    pub const INVALID_VALUE: Self = Self(0x0609_0030);
    /// Value of parameter written too high.
    pub const VALUE_HIGH: Self = Self(0x0609_0031);
    /// Value of parameter written too low.
    pub const VALUE_LOW: Self = Self(0x0609_0032);
    /// General error.
    pub const GENERAL: Self = Self(0x0800_0000);
    /// Data cannot be transferred or stored to the application.
    pub const DATA_TRANSF: Self = Self(0x0800_0020);
    /// Data cannot be transferred because of the present device state.
    pub const DATA_DEV_STATE: Self = Self(0x0800_0022);
    /// No data available.
    pub const NO_DATA: Self = Self(0x0800_0024);

    /// Human-readable description of well-known codes, for diagnostics.
    pub fn description(self) -> &'static str {
        match self {
            Self::TOGGLE_BIT => "toggle bit not alternated",
            Self::TIMEOUT => "SDO protocol timed out",
            Self::CMD_SPECIFIER => "command specifier not valid or unknown",
            Self::OUT_OF_MEMORY => "out of memory",
            Self::UNSUPPORTED_ACCESS => "unsupported access to object",
            Self::WRITE_ONLY => "attempt to read a write-only object",
            Self::READ_ONLY => "attempt to write a read-only object",
            Self::NO_OBJECT => "object does not exist in the object dictionary",
            Self::NO_MAP => "object cannot be mapped to the PDO",
            Self::TYPE_MISMATCH => "length of service parameter does not match",
            Self::DATA_LONG => "length of service parameter too high",
            Self::DATA_SHORT => "length of service parameter too short",
            Self::SUB_UNKNOWN => "sub-index does not exist",
            Self::INVALID_VALUE => "invalid value for parameter",
            Self::VALUE_HIGH => "value of parameter written too high",
            Self::VALUE_LOW => "value of parameter written too low",
            Self::GENERAL => "general error",
            Self::DATA_TRANSF => "data cannot be transferred or stored",
            Self::DATA_DEV_STATE => "data cannot be transferred (device state)",
            Self::NO_DATA => "no data available",
            _ => "unknown abort code",
        }
    }
}

impl core::fmt::Display for SdoAbortCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#010X} ({})", self.0, self.description())
    }
}
