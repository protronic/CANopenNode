/// A CANopen node id in the range 1..=127.
///
/// Mirrors the `nodeId` parameter of `CO_CANopenInit()`; the unconfigured
/// LSS node id (0xFF) is deliberately not representable — LSS address
/// claiming will get its own type when the LSS module is ported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u8);

impl NodeId {
    /// Create a node id, returning `None` unless `1 <= id <= 127`.
    pub const fn new(id: u8) -> Option<Self> {
        if id >= 1 && id <= 127 {
            Some(Self(id))
        } else {
            None
        }
    }

    /// The raw id value (1..=127).
    pub const fn raw(self) -> u8 {
        self.0
    }
}

impl core::fmt::Display for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_range() {
        assert!(NodeId::new(0).is_none());
        assert_eq!(NodeId::new(1).unwrap().raw(), 1);
        assert_eq!(NodeId::new(127).unwrap().raw(), 127);
        assert!(NodeId::new(128).is_none());
        assert!(NodeId::new(255).is_none());
    }
}
