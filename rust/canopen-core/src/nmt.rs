//! NMT (network management) slave state machine, port of `301/CO_NMT_Heartbeat.*`.

use crate::NodeId;

/// NMT node states as reported in the heartbeat message (CiA 301 §7.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NmtState {
    /// Device is booting; never reported on the bus (boot-up uses 0x00).
    Initializing = 0x00,
    /// NMT stopped: only NMT and heartbeat are active.
    Stopped = 0x04,
    /// Fully operational: PDO transfer is enabled.
    Operational = 0x05,
    /// Pre-operational: SDO configuration is possible, no PDO transfer.
    PreOperational = 0x7F,
}

impl NmtState {
    /// Whether SDO communication is active in this state.
    pub const fn sdo_active(self) -> bool {
        matches!(self, NmtState::PreOperational | NmtState::Operational)
    }

    /// Whether PDO transfer is active in this state.
    pub const fn pdo_active(self) -> bool {
        matches!(self, NmtState::Operational)
    }
}

/// NMT commands (CiA 301 §7.2.8.3.1), the first byte of an NMT service frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NmtCommand {
    /// Enter operational.
    Start = 0x01,
    /// Enter stopped.
    Stop = 0x02,
    /// Enter pre-operational.
    EnterPreOperational = 0x80,
    /// Reset application (full device reset).
    ResetNode = 0x81,
    /// Reset communication (CANopen re-initialization).
    ResetCommunication = 0x82,
}

impl NmtCommand {
    /// Decode the command byte of an NMT service frame.
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Self::Start),
            0x02 => Some(Self::Stop),
            0x80 => Some(Self::EnterPreOperational),
            0x81 => Some(Self::ResetNode),
            0x82 => Some(Self::ResetCommunication),
            _ => None,
        }
    }
}

/// Result of processing an NMT command that the application must act on,
/// port of `CO_NMT_reset_cmd_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NmtResetRequest {
    /// Reset CANopen communication (re-create the [`crate::Node`]).
    Communication,
    /// Reset the whole application/device.
    Node,
}

/// The NMT slave state machine of one local node.
#[derive(Debug)]
pub struct NmtSlave {
    node_id: NodeId,
    state: NmtState,
}

impl NmtSlave {
    /// Create the state machine in `Initializing`; call
    /// [`finish_boot`](Self::finish_boot) once initialization is done.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            state: NmtState::Initializing,
        }
    }

    /// Current NMT state.
    pub fn state(&self) -> NmtState {
        self.state
    }

    /// Complete initialization: transition to pre-operational (CiA 301
    /// requires entering pre-operational autonomously after boot-up).
    pub fn finish_boot(&mut self) {
        self.state = NmtState::PreOperational;
    }

    /// Handle an NMT service frame payload (`[command, addressed node id]`,
    /// node id 0 = all nodes). Returns a reset request the application must
    /// perform, if any. Malformed or foreign-addressed frames are ignored.
    pub fn on_nmt_frame(&mut self, data: &[u8]) -> Option<NmtResetRequest> {
        let [cmd, addressed] = *data else {
            return None;
        };
        if addressed != 0 && addressed != self.node_id.raw() {
            return None;
        }
        let cmd = NmtCommand::from_byte(cmd)?;
        self.apply(cmd)
    }

    /// Apply an NMT command directly (also used by the application, e.g. for
    /// self-starting nodes).
    pub fn apply(&mut self, cmd: NmtCommand) -> Option<NmtResetRequest> {
        match cmd {
            NmtCommand::Start => self.state = NmtState::Operational,
            NmtCommand::Stop => self.state = NmtState::Stopped,
            NmtCommand::EnterPreOperational => self.state = NmtState::PreOperational,
            NmtCommand::ResetNode => return Some(NmtResetRequest::Node),
            NmtCommand::ResetCommunication => return Some(NmtResetRequest::Communication),
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slave() -> NmtSlave {
        let mut s = NmtSlave::new(NodeId::new(10).unwrap());
        s.finish_boot();
        s
    }

    #[test]
    fn boot_enters_preoperational() {
        let mut s = NmtSlave::new(NodeId::new(10).unwrap());
        assert_eq!(s.state(), NmtState::Initializing);
        s.finish_boot();
        assert_eq!(s.state(), NmtState::PreOperational);
    }

    #[test]
    fn addressed_and_broadcast_commands() {
        let mut s = slave();
        assert_eq!(s.on_nmt_frame(&[0x01, 10]), None);
        assert_eq!(s.state(), NmtState::Operational);
        assert_eq!(s.on_nmt_frame(&[0x02, 0]), None); // broadcast
        assert_eq!(s.state(), NmtState::Stopped);
        assert_eq!(s.on_nmt_frame(&[0x80, 10]), None);
        assert_eq!(s.state(), NmtState::PreOperational);
    }

    #[test]
    fn foreign_and_malformed_frames_ignored() {
        let mut s = slave();
        assert_eq!(s.on_nmt_frame(&[0x01, 11]), None); // other node
        assert_eq!(s.state(), NmtState::PreOperational);
        assert_eq!(s.on_nmt_frame(&[0x01]), None); // short frame
        assert_eq!(s.on_nmt_frame(&[0x55, 10]), None); // unknown command
        assert_eq!(s.state(), NmtState::PreOperational);
    }

    #[test]
    fn reset_commands_are_reported() {
        let mut s = slave();
        assert_eq!(s.on_nmt_frame(&[0x81, 10]), Some(NmtResetRequest::Node));
        assert_eq!(
            s.on_nmt_frame(&[0x82, 0]),
            Some(NmtResetRequest::Communication)
        );
    }

    #[test]
    fn service_availability_per_state() {
        assert!(NmtState::PreOperational.sdo_active());
        assert!(!NmtState::PreOperational.pdo_active());
        assert!(NmtState::Operational.sdo_active());
        assert!(NmtState::Operational.pdo_active());
        assert!(!NmtState::Stopped.sdo_active());
        assert!(!NmtState::Stopped.pdo_active());
    }
}
