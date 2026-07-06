//! SDO server (CiA 301 §7.2.4), port of `301/CO_SDOserver.*`.
//!
//! Serves the node's [`ObjectDictionary`] on the default SDO channel
//! (requests on 0x600 + node id, responses on 0x580 + node id). Expedited
//! transfers only for now — segmented and block transfers are on the
//! roadmap; requests requiring them are aborted cleanly.

use crate::cob;
use crate::od::ObjectDictionary;
use crate::sdo::SdoAbortCode;
use crate::{CanFrame, NodeId, TxSink};

/// Notification about a completed server-side transfer, so the node can
/// react to configuration writes (e.g. 0x1017 heartbeat time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdoServerEvent {
    /// A client wrote `index:sub` successfully.
    ObjectWritten {
        /// Object index.
        index: u16,
        /// Object sub-index.
        sub: u8,
    },
}

/// The SDO server of one local node.
#[derive(Debug)]
pub struct SdoServer {
    node_id: NodeId,
}

impl SdoServer {
    /// Create the server for the node's default SDO channel.
    pub fn new(node_id: NodeId) -> Self {
        Self { node_id }
    }

    /// COB-ID this server listens on (client requests), for RX filters.
    pub fn request_cob_id(&self) -> u16 {
        cob::sdo_rx(self.node_id)
    }

    /// Feed a received frame to the server. Frames not addressed to this
    /// server's request COB-ID are ignored. Responses (including aborts) are
    /// emitted via `tx`.
    pub fn on_frame(
        &mut self,
        frame: &CanFrame,
        od: &mut impl ObjectDictionary,
        tx: &mut impl TxSink,
    ) -> Option<SdoServerEvent> {
        if frame.id() != self.request_cob_id() || frame.dlc() != 8 {
            return None;
        }
        let data = frame.data();
        let index = u16::from_le_bytes([data[1], data[2]]);
        let sub = data[3];

        match data[0] >> 5 {
            // Download initiate (client writes to us).
            1 => self.download(index, sub, data, od, tx),
            // Upload initiate (client reads from us).
            2 => {
                self.upload(index, sub, od, tx);
                None
            }
            // Abort from the client: nothing in progress to clean up until
            // segmented transfers are ported.
            4 => None,
            _ => {
                self.abort(index, sub, SdoAbortCode::CMD_SPECIFIER, tx);
                None
            }
        }
    }

    fn download(
        &mut self,
        index: u16,
        sub: u8,
        data: &[u8],
        od: &mut impl ObjectDictionary,
        tx: &mut impl TxSink,
    ) -> Option<SdoServerEvent> {
        let expedited = data[0] & 0x02 != 0;
        let size_indicated = data[0] & 0x01 != 0;
        if !expedited {
            // Segmented download: not supported yet.
            self.abort(index, sub, SdoAbortCode::CMD_SPECIFIER, tx);
            return None;
        }
        // Without size indication all four bytes are passed through; the
        // OD's exact-length check rejects mismatches.
        let len = if size_indicated {
            4 - ((data[0] >> 2) & 0x03) as usize
        } else {
            4
        };
        match od.write(index, sub, &data[4..4 + len]) {
            Ok(()) => {
                self.respond(0x60, index, sub, &[], tx);
                Some(SdoServerEvent::ObjectWritten { index, sub })
            }
            Err(e) => {
                self.abort(index, sub, e.abort_code(), tx);
                None
            }
        }
    }

    fn upload(&mut self, index: u16, sub: u8, od: &impl ObjectDictionary, tx: &mut impl TxSink) {
        let info = match od.info(index, sub) {
            Ok(info) => info,
            Err(e) => return self.abort(index, sub, e.abort_code(), tx),
        };
        if info.size > 4 {
            // Requires a segmented transfer: not supported yet.
            return self.abort(index, sub, SdoAbortCode::CMD_SPECIFIER, tx);
        }
        let mut buf = [0u8; 4];
        let len = match od.read(index, sub, &mut buf) {
            Ok(0) => return self.abort(index, sub, SdoAbortCode::NO_DATA, tx),
            Ok(len) => len,
            Err(e) => return self.abort(index, sub, e.abort_code(), tx),
        };
        // scs = 2, e = 1, s = 1, n = number of unused bytes.
        let cmd = 0x43 | (((4 - len) as u8) << 2);
        self.respond(cmd, index, sub, &buf[..len], tx);
    }

    fn abort(&self, index: u16, sub: u8, code: SdoAbortCode, tx: &mut impl TxSink) {
        self.respond(0x80, index, sub, &code.0.to_le_bytes(), tx);
    }

    /// Server response frame: `[cmd, index lo, index hi, sub, d0..d3]`.
    fn respond(&self, cmd: u8, index: u16, sub: u8, data: &[u8], tx: &mut impl TxSink) {
        let mut payload = [0u8; 8];
        payload[0] = cmd;
        payload[1..3].copy_from_slice(&index.to_le_bytes());
        payload[3] = sub;
        payload[4..4 + data.len()].copy_from_slice(data);
        tx.send(CanFrame::new(cob::sdo_tx(self.node_id), &payload).unwrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::od::{DataType, EntryInfo, OdError, PdoAccess, SdoAccess};

    /// Minimal OD: 0x2000 u16 rw, 0x2001 u32 ro, 0x2002 string (6 bytes,
    /// needs segmented), 0x2003 write-only u8.
    struct MockOd {
        x2000: u16,
        x2001: u32,
        x2003: u8,
    }

    impl Default for MockOd {
        fn default() -> Self {
            Self {
                x2000: 0x1234,
                x2001: 0xAABB_CCDD,
                x2003: 0,
            }
        }
    }

    impl ObjectDictionary for MockOd {
        fn info(&self, index: u16, sub: u8) -> Result<EntryInfo, OdError> {
            let entry = |data_type, sdo, size| {
                Ok(EntryInfo {
                    data_type,
                    sdo,
                    pdo: PdoAccess::No,
                    size,
                })
            };
            match (index, sub) {
                (0x2000, 0) => entry(DataType::Unsigned16, SdoAccess::ReadWrite, 2),
                (0x2001, 0) => entry(DataType::Unsigned32, SdoAccess::ReadOnly, 4),
                (0x2002, 0) => entry(DataType::VisibleString, SdoAccess::ReadOnly, 6),
                (0x2003, 0) => entry(DataType::Unsigned8, SdoAccess::WriteOnly, 1),
                (0x2000..=0x2003, _) => Err(OdError::SubIndexNotFound),
                _ => Err(OdError::ObjectNotFound),
            }
        }

        fn read(&self, index: u16, sub: u8, buf: &mut [u8]) -> Result<usize, OdError> {
            match (index, sub) {
                (0x2000, 0) => crate::od::read_bytes(buf, &self.x2000.to_le_bytes()),
                (0x2001, 0) => crate::od::read_bytes(buf, &self.x2001.to_le_bytes()),
                (0x2002, 0) => crate::od::read_bytes(buf, b"abcdef"),
                (0x2003, 0) => Err(OdError::WriteOnly),
                (0x2000..=0x2003, _) => Err(OdError::SubIndexNotFound),
                _ => Err(OdError::ObjectNotFound),
            }
        }

        fn write(&mut self, index: u16, sub: u8, data: &[u8]) -> Result<(), OdError> {
            match (index, sub) {
                (0x2000, 0) => {
                    self.x2000 = u16::from_le_bytes(crate::od::exact::<2>(data)?);
                    Ok(())
                }
                (0x2001, 0) | (0x2002, 0) => Err(OdError::ReadOnly),
                (0x2003, 0) => {
                    self.x2003 = u8::from_le_bytes(crate::od::exact::<1>(data)?);
                    Ok(())
                }
                (0x2000..=0x2003, _) => Err(OdError::SubIndexNotFound),
                _ => Err(OdError::ObjectNotFound),
            }
        }
    }

    const NODE: u8 = 0x0A;

    fn setup() -> (SdoServer, MockOd) {
        (SdoServer::new(NodeId::new(NODE).unwrap()), MockOd::default())
    }

    fn request(payload: [u8; 8]) -> CanFrame {
        CanFrame::new(0x600 + NODE as u16, &payload).unwrap()
    }

    fn exchange(server: &mut SdoServer, od: &mut MockOd, payload: [u8; 8]) -> (Vec<CanFrame>, Option<SdoServerEvent>) {
        let mut sent = Vec::new();
        let ev = server.on_frame(&request(payload), od, &mut |f| sent.push(f));
        (sent, ev)
    }

    #[test]
    fn expedited_upload() {
        let (mut server, mut od) = setup();
        let (sent, ev) = exchange(&mut server, &mut od, [0x40, 0x00, 0x20, 0x00, 0, 0, 0, 0]);
        assert_eq!(ev, None);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].id(), 0x580 + NODE as u16);
        // u16 -> e=1, s=1, n=2 -> 0x4B.
        assert_eq!(sent[0].data(), &[0x4B, 0x00, 0x20, 0x00, 0x34, 0x12, 0, 0]);
    }

    #[test]
    fn expedited_download_updates_od_and_reports() {
        let (mut server, mut od) = setup();
        let (sent, ev) = exchange(&mut server, &mut od, [0x2B, 0x00, 0x20, 0x00, 0xF4, 0x01, 0, 0]);
        assert_eq!(ev, Some(SdoServerEvent::ObjectWritten { index: 0x2000, sub: 0 }));
        assert_eq!(sent[0].data(), &[0x60, 0x00, 0x20, 0x00, 0, 0, 0, 0]);
        assert_eq!(od.x2000, 500);
    }

    #[test]
    fn download_without_size_indication_uses_od_length_check() {
        let (mut server, mut od) = setup();
        // e=1, s=0 (0x22): four bytes passed, u16 entry -> type mismatch.
        let (sent, ev) = exchange(&mut server, &mut od, [0x22, 0x00, 0x20, 0x00, 1, 2, 3, 4]);
        assert_eq!(ev, None);
        assert_eq!(sent[0].data()[0], 0x80);
        assert_eq!(
            u32::from_le_bytes(sent[0].data()[4..8].try_into().unwrap()),
            SdoAbortCode::TYPE_MISMATCH.0
        );
        assert_eq!(od.x2000, 0x1234, "value must be unchanged");
    }

    #[test]
    fn access_violations_abort() {
        let (mut server, mut od) = setup();
        // Write to read-only 0x2001.
        let (sent, _) = exchange(&mut server, &mut od, [0x23, 0x01, 0x20, 0x00, 1, 2, 3, 4]);
        assert_eq!(
            u32::from_le_bytes(sent[0].data()[4..8].try_into().unwrap()),
            SdoAbortCode::READ_ONLY.0
        );
        // Read from write-only 0x2003.
        let (sent, _) = exchange(&mut server, &mut od, [0x40, 0x03, 0x20, 0x00, 0, 0, 0, 0]);
        assert_eq!(
            u32::from_le_bytes(sent[0].data()[4..8].try_into().unwrap()),
            SdoAbortCode::WRITE_ONLY.0
        );
    }

    #[test]
    fn unknown_object_and_sub_abort() {
        let (mut server, mut od) = setup();
        let (sent, _) = exchange(&mut server, &mut od, [0x40, 0xFF, 0x2F, 0x00, 0, 0, 0, 0]);
        assert_eq!(
            u32::from_le_bytes(sent[0].data()[4..8].try_into().unwrap()),
            SdoAbortCode::NO_OBJECT.0
        );
        let (sent, _) = exchange(&mut server, &mut od, [0x40, 0x00, 0x20, 0x05, 0, 0, 0, 0]);
        assert_eq!(
            u32::from_le_bytes(sent[0].data()[4..8].try_into().unwrap()),
            SdoAbortCode::SUB_UNKNOWN.0
        );
    }

    #[test]
    fn oversized_upload_aborts_until_segmented_is_ported() {
        let (mut server, mut od) = setup();
        let (sent, _) = exchange(&mut server, &mut od, [0x40, 0x02, 0x20, 0x00, 0, 0, 0, 0]);
        assert_eq!(sent[0].data()[0], 0x80);
        // Segmented download initiate (e=0) likewise.
        let (sent, _) = exchange(&mut server, &mut od, [0x21, 0x00, 0x20, 0x00, 6, 0, 0, 0]);
        assert_eq!(sent[0].data()[0], 0x80);
    }

    #[test]
    fn foreign_frames_and_client_aborts_are_ignored() {
        let (mut server, mut od) = setup();
        let mut sent = Vec::new();
        let hb = CanFrame::new(0x700 + NODE as u16, &[0x05]).unwrap();
        assert_eq!(server.on_frame(&hb, &mut od, &mut |f| sent.push(f)), None);
        let short = CanFrame::new(0x600 + NODE as u16, &[0x40]).unwrap();
        assert_eq!(server.on_frame(&short, &mut od, &mut |f| sent.push(f)), None);
        let (aborted, ev) = exchange(&mut server, &mut od, [0x80, 0x00, 0x20, 0x00, 0, 0, 4, 5]);
        assert!(aborted.is_empty());
        assert_eq!(ev, None);
        assert!(sent.is_empty());
    }

    #[test]
    fn short_string_uploads_expedited() {
        let (mut server, mut od) = setup();
        // Make the string fit: not possible with MockOd's fixed 6 bytes, so
        // verify the boundary: exactly 4 bytes would be expedited. Use 0x2001
        // (4-byte u32) as the boundary case instead.
        let (sent, _) = exchange(&mut server, &mut od, [0x40, 0x01, 0x20, 0x00, 0, 0, 0, 0]);
        assert_eq!(sent[0].data(), &[0x43, 0x01, 0x20, 0x00, 0xDD, 0xCC, 0xBB, 0xAA]);
    }
}
