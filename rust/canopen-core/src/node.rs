//! Local node orchestration, the port of `CANopen.c` (`CO_new` /
//! `CO_CANopenInit` / `CO_process`).
//!
//! [`Node`] wires the protocol objects of one local CANopen device together
//! behind the same sans-IO calling convention: `on_frame` for received
//! frames, `process` for time-driven work, a [`TxSink`] for output. The node
//! owns its [`ObjectDictionary`] — single-task ownership replaces the
//! `CO_LOCK_OD` critical sections of the C stack.

use crate::cob;
use crate::heartbeat::HeartbeatProducer;
use crate::nmt::{NmtResetRequest, NmtSlave, NmtState};
use crate::od::ObjectDictionary;
use crate::sdo::{SdoServer, SdoServerEvent};
use crate::{CanFrame, Micros, NodeId, TxSink};

/// OD index of the producer heartbeat time (u16, milliseconds).
const OD_HEARTBEAT_TIME: u16 = 0x1017;

/// Reset request the application must perform, mirroring the return value of
/// `CO_process()` (`CO_NMT_reset_cmd_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetCommand {
    /// Re-initialize CANopen communication: drop this [`Node`], create a new
    /// one (with a freshly initialized OD) and call [`Node::start`] again.
    Communication,
    /// Reset the whole device (application-defined; on MCUs typically a
    /// system reset).
    Node,
}

/// One local CANopen device: NMT slave, heartbeat producer and SDO server
/// serving the object dictionary (EMCY, PDO and SYNC will be added as they
/// are ported).
#[derive(Debug)]
pub struct Node<OD: ObjectDictionary> {
    node_id: NodeId,
    nmt: NmtSlave,
    heartbeat: HeartbeatProducer,
    sdo: SdoServer,
    od: OD,
}

impl<OD: ObjectDictionary> Node<OD> {
    /// Create a node in the `Initializing` state. Communication parameters
    /// (producer heartbeat time, 0x1017) are taken from the OD.
    pub fn new(node_id: NodeId, od: OD) -> Self {
        let heartbeat_ms = read_u16(&od, OD_HEARTBEAT_TIME, 0).unwrap_or(0);
        Self {
            node_id,
            nmt: NmtSlave::new(node_id),
            heartbeat: HeartbeatProducer::new(node_id, heartbeat_ms),
            sdo: SdoServer::new(node_id),
            od,
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

    /// The node's object dictionary.
    pub fn od(&self) -> &OD {
        &self.od
    }

    /// Mutable access to the object dictionary for the application. After
    /// changing communication parameters (e.g. 0x1017), call
    /// [`refresh_comm_config`](Self::refresh_comm_config).
    pub fn od_mut(&mut self) -> &mut OD {
        &mut self.od
    }

    /// Re-read communication parameters from the OD (heartbeat time). Called
    /// automatically for SDO writes; needed only after direct OD writes by
    /// the application.
    pub fn refresh_comm_config(&mut self) {
        if let Some(ms) = read_u16(&self.od, OD_HEARTBEAT_TIME, 0) {
            self.heartbeat.set_period_ms(ms);
        }
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
    pub fn on_frame(
        &mut self,
        frame: &CanFrame,
        _now: Micros,
        tx: &mut impl TxSink,
    ) -> Option<ResetCommand> {
        match frame.id() {
            cob::NMT_SERVICE => match self.nmt.on_nmt_frame(frame.data()) {
                Some(NmtResetRequest::Communication) => Some(ResetCommand::Communication),
                Some(NmtResetRequest::Node) => Some(ResetCommand::Node),
                None => None,
            },
            id if id == self.sdo.request_cob_id() && self.nmt.state().sdo_active() => {
                let event = self.sdo.on_frame(frame, &mut self.od, tx);
                if let Some(SdoServerEvent::ObjectWritten {
                    index: OD_HEARTBEAT_TIME,
                    sub: 0,
                }) = event
                {
                    self.refresh_comm_config();
                }
                None
            }
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

/// Read a u16 entry from an OD, `None` if absent or unreadable.
fn read_u16(od: &impl ObjectDictionary, index: u16, sub: u8) -> Option<u16> {
    let mut buf = [0u8; 2];
    match od.read(index, sub, &mut buf) {
        Ok(2) => Some(u16::from_le_bytes(buf)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::od::{self, DataType, EntryInfo, OdError, PdoAccess, SdoAccess};

    /// Minimal OD with just the producer heartbeat time (0x1017, rw).
    struct TestOd {
        heartbeat_ms: u16,
    }

    impl ObjectDictionary for TestOd {
        fn info(&self, index: u16, sub: u8) -> Result<EntryInfo, OdError> {
            match (index, sub) {
                (0x1017, 0) => Ok(EntryInfo {
                    data_type: DataType::Unsigned16,
                    sdo: SdoAccess::ReadWrite,
                    pdo: PdoAccess::No,
                    size: 2,
                }),
                (0x1017, _) => Err(OdError::SubIndexNotFound),
                _ => Err(OdError::ObjectNotFound),
            }
        }

        fn read(&self, index: u16, sub: u8, buf: &mut [u8]) -> Result<usize, OdError> {
            match (index, sub) {
                (0x1017, 0) => od::read_bytes(buf, &self.heartbeat_ms.to_le_bytes()),
                (0x1017, _) => Err(OdError::SubIndexNotFound),
                _ => Err(OdError::ObjectNotFound),
            }
        }

        fn write(&mut self, index: u16, sub: u8, data: &[u8]) -> Result<(), OdError> {
            match (index, sub) {
                (0x1017, 0) => {
                    self.heartbeat_ms = u16::from_le_bytes(od::exact::<2>(data)?);
                    Ok(())
                }
                (0x1017, _) => Err(OdError::SubIndexNotFound),
                _ => Err(OdError::ObjectNotFound),
            }
        }
    }

    fn node(heartbeat_ms: u16) -> Node<TestOd> {
        Node::new(NodeId::new(3).unwrap(), TestOd { heartbeat_ms })
    }

    fn frames_of(v: &mut Vec<CanFrame>) -> impl FnMut(CanFrame) + '_ {
        move |f| v.push(f)
    }

    #[test]
    fn boot_sequence_and_heartbeat_lifecycle() {
        let mut node = node(50);
        let mut sent = Vec::new();

        node.start(0, &mut frames_of(&mut sent));
        assert_eq!(sent, [CanFrame::new(0x703, &[0x00]).unwrap()]);
        assert_eq!(node.nmt_state(), NmtState::PreOperational);

        // Heartbeat period comes from OD 0x1017: due after 50 ms.
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
        let mut node = node(0);
        let mut sent = Vec::new();
        node.start(0, &mut frames_of(&mut sent));

        let reset = CanFrame::new(0x000, &[0x82, 3]).unwrap();
        assert_eq!(
            node.on_frame(&reset, 1_000, &mut frames_of(&mut sent)),
            Some(ResetCommand::Communication)
        );
    }

    #[test]
    fn sdo_server_serves_od_and_heartbeat_write_takes_effect() {
        let mut node = node(0);
        let mut sent = Vec::new();
        node.start(0, &mut frames_of(&mut sent));
        assert_eq!(node.process(1_000_000, &mut frames_of(&mut sent)), None, "hb disabled");

        // SDO write 0x1017 = 100 ms.
        let req = CanFrame::new(0x603, &[0x2B, 0x17, 0x10, 0x00, 100, 0, 0, 0]).unwrap();
        sent.clear();
        node.on_frame(&req, 1_000_000, &mut frames_of(&mut sent));
        assert_eq!(sent[0].data()[0], 0x60, "SDO write response");
        assert_eq!(node.od().heartbeat_ms, 100);

        // The heartbeat producer picked the new period up immediately.
        sent.clear();
        node.process(1_000_000, &mut frames_of(&mut sent));
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].id(), 0x703);
        let next = node.process(1_050_000, &mut frames_of(&mut sent));
        assert_eq!(next, Some(1_100_000));

        // SDO read 0x1017 returns the new value.
        sent.clear();
        let req = CanFrame::new(0x603, &[0x40, 0x17, 0x10, 0x00, 0, 0, 0, 0]).unwrap();
        node.on_frame(&req, 1_060_000, &mut frames_of(&mut sent));
        assert_eq!(sent[0].data(), &[0x4B, 0x17, 0x10, 0x00, 100, 0, 0, 0]);
    }

    #[test]
    fn sdo_is_inactive_in_stopped_state() {
        let mut node = node(0);
        let mut sent = Vec::new();
        node.start(0, &mut frames_of(&mut sent));

        let stop = CanFrame::new(0x000, &[0x02, 3]).unwrap();
        node.on_frame(&stop, 0, &mut frames_of(&mut sent));
        assert_eq!(node.nmt_state(), NmtState::Stopped);

        sent.clear();
        let req = CanFrame::new(0x603, &[0x40, 0x17, 0x10, 0x00, 0, 0, 0, 0]).unwrap();
        node.on_frame(&req, 0, &mut frames_of(&mut sent));
        assert!(sent.is_empty(), "no SDO response in stopped state");
    }
}
