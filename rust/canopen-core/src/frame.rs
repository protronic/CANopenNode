//! Classic CAN data frame as used by CANopen.
//!
//! CANopen (CiA 301) uses standard 11-bit identifiers and 0..=8 byte data
//! frames exclusively, so this type is deliberately simpler than a general
//! CAN frame: no extended ids, no remote frames, no CAN-FD. Adapters convert
//! at the bus boundary (see the `embedded-can` feature and the
//! `canopen-socketcan` crate).

/// A classic CAN data frame with a standard (11-bit) identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanFrame {
    id: u16,
    len: u8,
    data: [u8; 8],
}

impl CanFrame {
    /// Create a frame. Returns `None` if `id > 0x7FF` or `data.len() > 8`.
    pub fn new(id: u16, data: &[u8]) -> Option<Self> {
        if id > 0x7FF || data.len() > 8 {
            return None;
        }
        let mut buf = [0u8; 8];
        buf[..data.len()].copy_from_slice(data);
        Some(Self {
            id,
            len: data.len() as u8,
            data: buf,
        })
    }

    /// The 11-bit CAN identifier (COB-ID).
    pub const fn id(&self) -> u16 {
        self.id
    }

    /// The frame payload (0..=8 bytes).
    pub fn data(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Payload length in bytes (DLC), 0..=8.
    pub const fn dlc(&self) -> u8 {
        self.len
    }
}

#[cfg(feature = "embedded-can")]
mod embedded_can_impl {
    use super::CanFrame;
    use embedded_can::{Frame, Id, StandardId};

    impl CanFrame {
        /// Convert from any [`embedded_can::Frame`] implementor
        /// (embassy-stm32 `Frame`, socketcan `CanFrame`, ...).
        ///
        /// Returns `None` for frames CANopen never uses: extended-id frames
        /// and remote frames.
        pub fn from_embedded<F: Frame>(frame: &F) -> Option<Self> {
            if frame.is_remote_frame() {
                return None;
            }
            match frame.id() {
                Id::Standard(id) => Self::new(id.as_raw(), frame.data()),
                Id::Extended(_) => None,
            }
        }

        /// Convert into any [`embedded_can::Frame`] implementor.
        pub fn to_embedded<F: Frame>(&self) -> F {
            // Both invariants (valid 11-bit id, len <= 8) are guaranteed by
            // construction, so the unwraps cannot fire.
            let id = StandardId::new(self.id).unwrap();
            F::new(id, self.data()).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_limits() {
        assert!(CanFrame::new(0x7FF, &[0; 8]).is_some());
        assert!(CanFrame::new(0x800, &[]).is_none());
        assert!(CanFrame::new(0, &[0; 9]).is_none());
    }

    #[test]
    fn payload_roundtrip() {
        let f = CanFrame::new(0x601, &[0x40, 0x17, 0x10, 0x00]).unwrap();
        assert_eq!(f.id(), 0x601);
        assert_eq!(f.dlc(), 4);
        assert_eq!(f.data(), &[0x40, 0x17, 0x10, 0x00]);
    }
}
