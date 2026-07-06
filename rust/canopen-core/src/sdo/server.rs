//! SDO server (CiA 301 §7.2.4), port of `301/CO_SDOserver.*`.
//!
//! Serves the node's [`ObjectDictionary`] on the default SDO channel
//! (requests on 0x600 + node id, responses on 0x580 + node id). Expedited
//! and segmented transfers are supported (block transfer is on the
//! roadmap). Segmented data is staged in an internal buffer of `BUF` bytes
//! (default 256): uploads read the whole value once, downloads accumulate
//! and write once — the OD keeps its simple exact-length interface.

use crate::cob;
use crate::od::ObjectDictionary;
use crate::sdo::SdoAbortCode;
use crate::{CanFrame, Micros, NodeId, TxSink};

/// Default server-side transfer timeout (1 s, the customary CANopenNode
/// value). Measured per client action, not per transfer.
pub const DEFAULT_SDO_SERVER_TIMEOUT_US: u64 = 1_000_000;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    /// Streaming a value from the buffer to the client.
    UploadSegments { len: usize, offset: usize, toggle: bool },
    /// Accumulating segments into the buffer before the OD write.
    DownloadSegments { indicated: Option<u32>, received: usize, toggle: bool },
}

/// The SDO server of one local node.
#[derive(Debug)]
pub struct SdoServer<const BUF: usize = 256> {
    node_id: NodeId,
    timeout_us: u64,
    state: State,
    index: u16,
    sub: u8,
    since: Micros,
    buf: [u8; BUF],
}

impl<const BUF: usize> SdoServer<BUF> {
    /// Create the server for the node's default SDO channel.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            timeout_us: DEFAULT_SDO_SERVER_TIMEOUT_US,
            state: State::Idle,
            index: 0,
            sub: 0,
            since: 0,
            buf: [0; BUF],
        }
    }

    /// Change the transfer timeout.
    pub fn set_timeout_us(&mut self, timeout_us: u64) {
        self.timeout_us = timeout_us;
    }

    /// COB-ID this server listens on (client requests), for RX filters.
    pub fn request_cob_id(&self) -> u16 {
        cob::sdo_rx(self.node_id)
    }

    /// Feed a received frame to the server. Frames not addressed to this
    /// server's request COB-ID are ignored. Responses (including aborts and
    /// segment acknowledgements) are emitted via `tx`.
    pub fn on_frame(
        &mut self,
        frame: &CanFrame,
        now: Micros,
        od: &mut impl ObjectDictionary,
        tx: &mut impl TxSink,
    ) -> Option<SdoServerEvent> {
        if frame.id() != self.request_cob_id() || frame.dlc() != 8 {
            return None;
        }
        let data: [u8; 8] = frame.data().try_into().unwrap();
        self.since = now;

        match data[0] >> 5 {
            // Download segment (only valid during a segmented download).
            0 => self.download_segment(data, od, tx),
            // Download initiate: implicitly cancels any stale transfer.
            1 => {
                self.state = State::Idle;
                self.set_object(&data);
                self.download_initiate(data, od, tx)
            }
            // Upload initiate.
            2 => {
                self.state = State::Idle;
                self.set_object(&data);
                self.upload_initiate(od, tx);
                None
            }
            // Upload segment request.
            3 => {
                self.upload_segment(data, tx);
                None
            }
            // Abort from the client: drop any transfer in progress.
            4 => {
                self.state = State::Idle;
                None
            }
            _ => {
                self.set_object(&data);
                self.abort(SdoAbortCode::CMD_SPECIFIER, tx);
                None
            }
        }
    }

    fn download_initiate(
        &mut self,
        data: [u8; 8],
        od: &mut impl ObjectDictionary,
        tx: &mut impl TxSink,
    ) -> Option<SdoServerEvent> {
        let expedited = data[0] & 0x02 != 0;
        let size_indicated = data[0] & 0x01 != 0;
        if expedited {
            // Without size indication all four bytes are passed through; the
            // OD's exact-length check rejects mismatches.
            let len = if size_indicated {
                4 - ((data[0] >> 2) & 0x03) as usize
            } else {
                4
            };
            return match od.write(self.index, self.sub, &data[4..4 + len]) {
                Ok(()) => {
                    self.respond(0x60, &[], tx);
                    Some(SdoServerEvent::ObjectWritten { index: self.index, sub: self.sub })
                }
                Err(e) => {
                    self.abort(e.abort_code(), tx);
                    None
                }
            };
        }

        // Segmented download. Check access and capacity up front so the
        // client fails fast instead of after transferring everything.
        match od.info(self.index, self.sub) {
            Ok(info) if !info.sdo.writable() => {
                self.abort(crate::od::OdError::ReadOnly.abort_code(), tx);
                return None;
            }
            Ok(_) => {}
            Err(e) => {
                self.abort(e.abort_code(), tx);
                return None;
            }
        }
        let indicated = size_indicated.then(|| u32::from_le_bytes([data[4], data[5], data[6], data[7]]));
        if indicated.is_some_and(|size| size as usize > BUF) {
            self.abort(SdoAbortCode::OUT_OF_MEMORY, tx);
            return None;
        }
        self.state = State::DownloadSegments { indicated, received: 0, toggle: false };
        self.respond(0x60, &[], tx);
        None
    }

    fn download_segment(
        &mut self,
        data: [u8; 8],
        od: &mut impl ObjectDictionary,
        tx: &mut impl TxSink,
    ) -> Option<SdoServerEvent> {
        let State::DownloadSegments { indicated, received, toggle } = self.state else {
            self.abort(SdoAbortCode::CMD_SPECIFIER, tx);
            return None;
        };
        if (data[0] >> 4) & 1 != toggle as u8 {
            self.abort(SdoAbortCode::TOGGLE_BIT, tx);
            return None;
        }
        let unused = ((data[0] >> 1) & 0x07) as usize;
        let chunk = 7 - unused;
        let last = data[0] & 0x01 != 0;
        if received + chunk > BUF {
            self.abort(SdoAbortCode::OUT_OF_MEMORY, tx);
            return None;
        }
        self.buf[received..received + chunk].copy_from_slice(&data[1..1 + chunk]);
        let received = received + chunk;

        if last {
            match indicated {
                Some(size) if (size as usize) > received => {
                    self.abort(SdoAbortCode::DATA_SHORT, tx);
                    return None;
                }
                Some(size) if (size as usize) < received => {
                    self.abort(SdoAbortCode::DATA_LONG, tx);
                    return None;
                }
                _ => {}
            }
            return match od.write(self.index, self.sub, &self.buf[..received]) {
                Ok(()) => {
                    self.state = State::Idle;
                    self.respond(0x20 | (u8::from(toggle) << 4), &[], tx);
                    Some(SdoServerEvent::ObjectWritten { index: self.index, sub: self.sub })
                }
                Err(e) => {
                    self.abort(e.abort_code(), tx);
                    None
                }
            };
        }
        self.state = State::DownloadSegments { indicated, received, toggle: !toggle };
        self.respond(0x20 | (u8::from(toggle) << 4), &[], tx);
        None
    }

    fn upload_initiate(&mut self, od: &impl ObjectDictionary, tx: &mut impl TxSink) {
        if let Err(e) = od.info(self.index, self.sub) {
            return self.abort(e.abort_code(), tx);
        }
        let len = match od.read(self.index, self.sub, &mut self.buf) {
            Ok(0) => return self.abort(SdoAbortCode::NO_DATA, tx),
            Ok(len) => len,
            Err(crate::od::OdError::BufferTooSmall) => {
                return self.abort(SdoAbortCode::OUT_OF_MEMORY, tx)
            }
            Err(e) => return self.abort(e.abort_code(), tx),
        };
        if len <= 4 {
            // Expedited response: scs=2, e=1, s=1, n = unused bytes.
            let cmd = 0x43 | (((4 - len) as u8) << 2);
            let data = [self.buf[0], self.buf[1], self.buf[2], self.buf[3]];
            self.respond(cmd, &data[..len], tx);
        } else {
            // Segmented response: scs=2, e=0, s=1, size announced.
            self.state = State::UploadSegments { len, offset: 0, toggle: false };
            self.respond(0x41, &(len as u32).to_le_bytes(), tx);
        }
    }

    fn upload_segment(&mut self, data: [u8; 8], tx: &mut impl TxSink) {
        let State::UploadSegments { len, offset, toggle } = self.state else {
            self.abort(SdoAbortCode::CMD_SPECIFIER, tx);
            return;
        };
        if (data[0] >> 4) & 1 != toggle as u8 {
            return self.abort(SdoAbortCode::TOGGLE_BIT, tx);
        }
        let chunk = (len - offset).min(7);
        let last = offset + chunk == len;
        // Segment response: scs=0, toggle, n = unused bytes, c = last.
        let cmd = (u8::from(toggle) << 4) | (((7 - chunk) as u8) << 1) | u8::from(last);
        let mut payload = [0u8; 8];
        payload[0] = cmd;
        payload[1..1 + chunk].copy_from_slice(&self.buf[offset..offset + chunk]);
        tx.send(CanFrame::new(cob::sdo_tx(self.node_id), &payload).unwrap());

        self.state = if last {
            State::Idle
        } else {
            State::UploadSegments { len, offset: offset + chunk, toggle: !toggle }
        };
    }

    /// Check for a client timeout during a segmented transfer. On timeout an
    /// abort is transmitted and the transfer dropped.
    pub fn poll(&mut self, now: Micros, tx: &mut impl TxSink) {
        if self.state == State::Idle {
            return;
        }
        if now.saturating_sub(self.since) >= self.timeout_us {
            self.abort(SdoAbortCode::TIMEOUT, tx);
        }
    }

    /// The next deadline at which [`poll`](Self::poll) should run, if a
    /// segmented transfer is in progress.
    pub fn next_deadline(&self) -> Option<Micros> {
        (self.state != State::Idle).then(|| self.since + self.timeout_us)
    }

    fn set_object(&mut self, data: &[u8; 8]) {
        self.index = u16::from_le_bytes([data[1], data[2]]);
        self.sub = data[3];
    }

    fn abort(&mut self, code: SdoAbortCode, tx: &mut impl TxSink) {
        self.state = State::Idle;
        self.respond(0x80, &code.0.to_le_bytes(), tx);
    }

    /// Server response frame addressing the current object:
    /// `[cmd, index lo, index hi, sub, d0..d3]`.
    fn respond(&self, cmd: u8, data: &[u8], tx: &mut impl TxSink) {
        let mut payload = [0u8; 8];
        payload[0] = cmd;
        payload[1..3].copy_from_slice(&self.index.to_le_bytes());
        payload[3] = self.sub;
        payload[4..4 + data.len()].copy_from_slice(data);
        tx.send(CanFrame::new(cob::sdo_tx(self.node_id), &payload).unwrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::od::{self, DataType, EntryInfo, ObjectDictionary, OdError, OdString, PdoAccess, SdoAccess};

    /// Minimal OD: 0x2000 u16 rw, 0x2001 u32 ro, 0x2002 string rw (cap 16),
    /// 0x2003 write-only u8.
    struct MockOd {
        x2000: u16,
        x2001: u32,
        x2002: OdString<16>,
        x2003: u8,
    }

    impl Default for MockOd {
        fn default() -> Self {
            Self {
                x2000: 0x1234,
                x2001: 0xAABB_CCDD,
                x2002: OdString::new(b"Hello CAN!"),
                x2003: 0,
            }
        }
    }

    impl ObjectDictionary for MockOd {
        fn info(&self, index: u16, sub: u8) -> Result<EntryInfo, OdError> {
            let entry = |data_type, sdo, size| Ok(EntryInfo { data_type, sdo, pdo: PdoAccess::No, size });
            match (index, sub) {
                (0x2000, 0) => entry(DataType::Unsigned16, SdoAccess::ReadWrite, 2),
                (0x2001, 0) => entry(DataType::Unsigned32, SdoAccess::ReadOnly, 4),
                (0x2002, 0) => entry(DataType::VisibleString, SdoAccess::ReadWrite, self.x2002.len()),
                (0x2003, 0) => entry(DataType::Unsigned8, SdoAccess::WriteOnly, 1),
                (0x2000..=0x2003, _) => Err(OdError::SubIndexNotFound),
                _ => Err(OdError::ObjectNotFound),
            }
        }

        fn read(&self, index: u16, sub: u8, buf: &mut [u8]) -> Result<usize, OdError> {
            match (index, sub) {
                (0x2000, 0) => od::read_bytes(buf, &self.x2000.to_le_bytes()),
                (0x2001, 0) => od::read_bytes(buf, &self.x2001.to_le_bytes()),
                (0x2002, 0) => od::read_bytes(buf, self.x2002.as_bytes()),
                (0x2003, 0) => Err(OdError::WriteOnly),
                (0x2000..=0x2003, _) => Err(OdError::SubIndexNotFound),
                _ => Err(OdError::ObjectNotFound),
            }
        }

        fn write(&mut self, index: u16, sub: u8, data: &[u8]) -> Result<(), OdError> {
            match (index, sub) {
                (0x2000, 0) => {
                    self.x2000 = u16::from_le_bytes(od::exact::<2>(data)?);
                    Ok(())
                }
                (0x2001, 0) => Err(OdError::ReadOnly),
                (0x2002, 0) => self.x2002.set(data),
                (0x2003, 0) => {
                    self.x2003 = u8::from_le_bytes(od::exact::<1>(data)?);
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

    fn exchange(
        server: &mut SdoServer,
        od: &mut MockOd,
        now: Micros,
        payload: [u8; 8],
    ) -> (Vec<CanFrame>, Option<SdoServerEvent>) {
        let mut sent = Vec::new();
        let ev = server.on_frame(&request(payload), now, od, &mut |f| sent.push(f));
        (sent, ev)
    }

    fn abort_code(frame: &CanFrame) -> u32 {
        assert_eq!(frame.data()[0], 0x80, "expected abort frame");
        u32::from_le_bytes(frame.data()[4..8].try_into().unwrap())
    }

    #[test]
    fn expedited_upload_and_download() {
        let (mut server, mut od) = setup();
        let (sent, _) = exchange(&mut server, &mut od, 0, [0x40, 0x00, 0x20, 0x00, 0, 0, 0, 0]);
        assert_eq!(sent[0].data(), &[0x4B, 0x00, 0x20, 0x00, 0x34, 0x12, 0, 0]);

        let (sent, ev) = exchange(&mut server, &mut od, 1, [0x2B, 0x00, 0x20, 0x00, 0xF4, 0x01, 0, 0]);
        assert_eq!(ev, Some(SdoServerEvent::ObjectWritten { index: 0x2000, sub: 0 }));
        assert_eq!(sent[0].data(), &[0x60, 0x00, 0x20, 0x00, 0, 0, 0, 0]);
        assert_eq!(od.x2000, 500);
    }

    #[test]
    fn segmented_upload_of_a_string() {
        let (mut server, mut od) = setup();
        // Initiate: 10-byte value -> segmented, size announced.
        let (sent, _) = exchange(&mut server, &mut od, 0, [0x40, 0x02, 0x20, 0x00, 0, 0, 0, 0]);
        assert_eq!(sent[0].data(), &[0x41, 0x02, 0x20, 0x00, 10, 0, 0, 0]);

        // Segment request toggle 0 -> 7 bytes, not last.
        let (sent, _) = exchange(&mut server, &mut od, 1, [0x60, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(sent[0].data(), &[0x00, b'H', b'e', b'l', b'l', b'o', b' ', b'C']);

        // Segment request toggle 1 -> 3 bytes (n=4), last.
        let (sent, _) = exchange(&mut server, &mut od, 2, [0x70, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(sent[0].data(), &[0x19, b'A', b'N', b'!', 0, 0, 0, 0]);
    }

    #[test]
    fn segmented_download_writes_od_once_complete() {
        let (mut server, mut od) = setup();
        // Initiate segmented download to the string: size 12.
        let (sent, ev) = exchange(&mut server, &mut od, 0, [0x21, 0x02, 0x20, 0x00, 12, 0, 0, 0]);
        assert_eq!(ev, None);
        assert_eq!(sent[0].data(), &[0x60, 0x02, 0x20, 0x00, 0, 0, 0, 0]);

        // Segment 1: toggle 0, 7 bytes.
        let (sent, ev) = exchange(&mut server, &mut od, 1, [0x00, b'p', b'r', b'o', b't', b'r', b'o', b'n']);
        assert_eq!(ev, None);
        assert_eq!(sent[0].data()[0], 0x20);
        assert_eq!(od.x2002.as_bytes(), b"Hello CAN!", "not written yet");

        // Segment 2: toggle 1, 5 bytes (n=2), last.
        let (sent, ev) = exchange(&mut server, &mut od, 2, [0x15, b'i', b'c', b'.', b'd', b'e', 0, 0]);
        assert_eq!(ev, Some(SdoServerEvent::ObjectWritten { index: 0x2002, sub: 0 }));
        assert_eq!(sent[0].data()[0], 0x30);
        assert_eq!(od.x2002.as_bytes(), b"protronic.de");
    }

    #[test]
    fn segmented_download_data_too_long_for_entry() {
        let (mut server, mut od) = setup();
        // 20 bytes: fits the server buffer, exceeds OdString<16>.
        exchange(&mut server, &mut od, 0, [0x21, 0x02, 0x20, 0x00, 20, 0, 0, 0]);
        exchange(&mut server, &mut od, 1, [0x00, 0, 1, 2, 3, 4, 5, 6]);
        exchange(&mut server, &mut od, 2, [0x10, 7, 8, 9, 10, 11, 12, 13]);
        let (sent, ev) = exchange(&mut server, &mut od, 3, [0x03, 14, 15, 16, 17, 18, 19, 0]);
        assert_eq!(ev, None);
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::DATA_LONG.0);
        assert_eq!(od.x2002.as_bytes(), b"Hello CAN!", "unchanged");
    }

    #[test]
    fn toggle_error_aborts_download() {
        let (mut server, mut od) = setup();
        exchange(&mut server, &mut od, 0, [0x21, 0x02, 0x20, 0x00, 12, 0, 0, 0]);
        // First segment arrives with toggle 1 instead of 0.
        let (sent, _) = exchange(&mut server, &mut od, 1, [0x10, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::TOGGLE_BIT.0);
        // Transfer is gone: a further segment is a protocol error.
        let (sent, _) = exchange(&mut server, &mut od, 2, [0x00, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::CMD_SPECIFIER.0);
    }

    #[test]
    fn segmented_download_to_readonly_fails_fast() {
        let (mut server, mut od) = setup();
        let (sent, _) = exchange(&mut server, &mut od, 0, [0x21, 0x01, 0x20, 0x00, 8, 0, 0, 0]);
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::READ_ONLY.0);
    }

    #[test]
    fn download_exceeding_buffer_aborts_out_of_memory() {
        let (mut server, mut od) = setup();
        let size = 300u32.to_le_bytes();
        let (sent, _) = exchange(
            &mut server,
            &mut od,
            0,
            [0x21, 0x02, 0x20, 0x00, size[0], size[1], size[2], size[3]],
        );
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::OUT_OF_MEMORY.0);
    }

    #[test]
    fn transfer_timeout_aborts() {
        let (mut server, mut od) = setup();
        exchange(&mut server, &mut od, 0, [0x21, 0x02, 0x20, 0x00, 12, 0, 0, 0]);
        assert_eq!(server.next_deadline(), Some(DEFAULT_SDO_SERVER_TIMEOUT_US));

        let mut sent = Vec::new();
        server.poll(999_999, &mut |f| sent.push(f));
        assert!(sent.is_empty());
        server.poll(1_000_000, &mut |f| sent.push(f));
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::TIMEOUT.0);
        assert_eq!(server.next_deadline(), None);
    }

    #[test]
    fn access_violations_abort() {
        let (mut server, mut od) = setup();
        let (sent, _) = exchange(&mut server, &mut od, 0, [0x23, 0x01, 0x20, 0x00, 1, 2, 3, 4]);
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::READ_ONLY.0);
        let (sent, _) = exchange(&mut server, &mut od, 1, [0x40, 0x03, 0x20, 0x00, 0, 0, 0, 0]);
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::WRITE_ONLY.0);
    }

    #[test]
    fn unknown_object_and_sub_abort() {
        let (mut server, mut od) = setup();
        let (sent, _) = exchange(&mut server, &mut od, 0, [0x40, 0xFF, 0x2F, 0x00, 0, 0, 0, 0]);
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::NO_OBJECT.0);
        let (sent, _) = exchange(&mut server, &mut od, 1, [0x40, 0x00, 0x20, 0x05, 0, 0, 0, 0]);
        assert_eq!(abort_code(&sent[0]), SdoAbortCode::SUB_UNKNOWN.0);
    }

    #[test]
    fn foreign_frames_and_client_aborts_are_ignored() {
        let (mut server, mut od) = setup();
        let mut sent = Vec::new();
        let hb = CanFrame::new(0x700 + NODE as u16, &[0x05]).unwrap();
        assert_eq!(server.on_frame(&hb, 0, &mut od, &mut |f| sent.push(f)), None);
        let short = CanFrame::new(0x600 + NODE as u16, &[0x40]).unwrap();
        assert_eq!(server.on_frame(&short, 0, &mut od, &mut |f| sent.push(f)), None);
        let (aborted, ev) = exchange(&mut server, &mut od, 1, [0x80, 0x00, 0x20, 0x00, 0, 0, 4, 5]);
        assert!(aborted.is_empty());
        assert_eq!(ev, None);
        assert!(sent.is_empty());
    }
}
