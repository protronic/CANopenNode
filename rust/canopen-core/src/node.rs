//! Local node orchestration, the port of `CANopen.c` (`CO_new` /
//! `CO_CANopenInit` / `CO_process`).
//!
//! [`Node`] wires the protocol objects of one local CANopen device together
//! behind the same sans-IO calling convention: `on_frame` for received
//! frames, `process` for time-driven work, a [`TxSink`] for output.

use crate::cob;
use crate::heartbeat::HeartbeatProducer;
use crate::nmt::{NmtResetRequest, NmtSlave, NmtState};
use crate::{CanFrame, Micros, NodeId, TxSink};

/// Reset request the application must perform, mirroring the return value of
/// `CO_process()` (`CO_NMT_reset_cmd_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetCommand {
    /// Re-initialize CANopen communication: drop this [`Node`], create a new
    /// one and call [`Node::start`] again.
    Communication,
    /// Reset the whole device (application-defined; on MCUs typically a
    /// system reset).
    Node,
}

/// Static configuration of a [`Node`].
#[derive(Debug, Clone, Copy)]
pub struct NodeConfig {
    /// Producer heartbeat time in milliseconds (OD 0x1017); 0 disables it.
    pub heartbeat_period_ms: u16,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            heartbeat_period_ms: 1000,
        }
    }
}

/// One local CANopen device: NMT slave plus heartbeat producer (more objects
/// — SDO server, EMCY, PDO, SYNC — will be added as they are ported).
#[derive(Debug)]
pub struct Node {
    node_id: NodeId,
    nmt: NmtSlave,
    heartbeat: HeartbeatProducer,
}

impl Node {
    /// Create a node in the `Initializing` state.
    pub fn new(node_id: NodeId, config: NodeConfig) -> Self {
        Self {
            node_id,
            nmt: NmtSlave::new(node_id),
            heartbeat: HeartbeatProducer::new(node_id, config.heartbeat_period_ms),
        }
    }

    /// The node's id.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Current NMT state.
    pub fn nmt_state(&self) -> NmtState {
        self.nmt.state()
    }

    /// Finish initialization: emits the boot-up message and enters
    /// pre-operational. Call once after creating the node (and after every
    /// communication reset).
    pub fn start(&mut self, now: Micros, tx: &mut impl TxSink) {
        self.heartbeat.send_boot_up(now, tx);
        self.nmt.finish_boot();
    }

    /// Feed one received CAN frame into the node. Returns a reset request
    /// the application must carry out, if any.
    pub fn on_frame(&mut self, frame: &CanFrame, _now: Micros, _tx: &mut impl TxSink) -> Option<ResetCommand> {
        match frame.id() {
            cob::NMT_SERVICE => match self.nmt.on_nmt_frame(frame.data()) {
                Some(NmtResetRequest::Communication) => Some(ResetCommand::Communication),
                Some(NmtResetRequest::Node) => Some(ResetCommand::Node),
                None => None,
            },
            _ => None,
        }
    }

    /// Run time-driven work (currently the heartbeat producer). Returns the
    /// next deadline at which `process` wants to run again, if any — the
    /// `timerNext_us` mechanism of `CO_process()`.
    pub fn process(&mut self, now: Micros, tx: &mut impl TxSink) -> Option<Micros> {
        self.heartbeat.process(self.nmt.state(), now, tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_of(v: &mut Vec<CanFrame>) -> impl FnMut(CanFrame) + '_ {
        move |f| v.push(f)
    }

    #[test]
    fn boot_sequence_and_heartbeat_lifecycle() {
        let mut node = Node::new(NodeId::new(3).unwrap(), NodeConfig { heartbeat_period_ms: 50 });
        let mut sent = Vec::new();

        node.start(0, &mut frames_of(&mut sent));
        assert_eq!(sent, [CanFrame::new(0x703, &[0x00]).unwrap()]);
        assert_eq!(node.nmt_state(), NmtState::PreOperational);

        // Heartbeat after 50 ms reports pre-operational.
        let next = node.process(50_000, &mut frames_of(&mut sent));
        assert_eq!(next, Some(100_000));
        assert_eq!(*sent.last().unwrap(), CanFrame::new(0x703, &[0x7F]).unwrap());

        // NMT start via broadcast, next heartbeat reports operational.
        let nmt_start = CanFrame::new(0x000, &[0x01, 0x00]).unwrap();
        assert_eq!(node.on_frame(&nmt_start, 60_000, &mut frames_of(&mut sent)), None);
        assert_eq!(node.nmt_state(), NmtState::Operational);
        node.process(100_000, &mut frames_of(&mut sent));
        assert_eq!(*sent.last().unwrap(), CanFrame::new(0x703, &[0x05]).unwrap());
    }

    #[test]
    fn reset_communication_is_surfaced() {
        let mut node = Node::new(NodeId::new(3).unwrap(), NodeConfig::default());
        let mut sent = Vec::new();
        node.start(0, &mut frames_of(&mut sent));

        let reset = CanFrame::new(0x000, &[0x82, 3]).unwrap();
        assert_eq!(
            node.on_frame(&reset, 1_000, &mut frames_of(&mut sent)),
            Some(ResetCommand::Communication)
        );
    }
}
