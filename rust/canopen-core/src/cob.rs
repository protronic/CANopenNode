//! Default COB-IDs (communication object identifiers) per CiA 301,
//! the "pre-defined connection set".
//!
//! Mirrors `CO_Default_CAN_ID_t` in `301/CO_ODinterface.h`.

use crate::NodeId;

/// NMT service (master -> all slaves), id 0x000.
pub const NMT_SERVICE: u16 = 0x000;
/// SYNC object, id 0x080.
pub const SYNC: u16 = 0x080;
/// TIME object, id 0x100.
pub const TIME: u16 = 0x100;

/// Emergency object of a node: 0x080 + node id.
pub const fn emcy(node: NodeId) -> u16 {
    0x080 + node.raw() as u16
}

/// First transmit PDO of a node: 0x180 + node id.
pub const fn tpdo1(node: NodeId) -> u16 {
    0x180 + node.raw() as u16
}

/// First receive PDO of a node: 0x200 + node id.
pub const fn rpdo1(node: NodeId) -> u16 {
    0x200 + node.raw() as u16
}

/// SDO response, server -> client ("tx" from the server's view): 0x580 + server node id.
pub const fn sdo_tx(server: NodeId) -> u16 {
    0x580 + server.raw() as u16
}

/// SDO request, client -> server ("rx" from the server's view): 0x600 + server node id.
pub const fn sdo_rx(server: NodeId) -> u16 {
    0x600 + server.raw() as u16
}

/// Heartbeat / boot-up message of a node: 0x700 + node id.
pub const fn heartbeat(node: NodeId) -> u16 {
    0x700 + node.raw() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_connection_set() {
        let n = NodeId::new(0x0A).unwrap();
        assert_eq!(emcy(n), 0x08A);
        assert_eq!(tpdo1(n), 0x18A);
        assert_eq!(rpdo1(n), 0x20A);
        assert_eq!(sdo_tx(n), 0x58A);
        assert_eq!(sdo_rx(n), 0x60A);
        assert_eq!(heartbeat(n), 0x70A);
    }
}
