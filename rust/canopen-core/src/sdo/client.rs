//! SDO client (CiA 301 §7.2.4), port of `301/CO_SDOclient.*`.
//!
//! Sans-IO: requests return the frame to transmit, responses are pushed in
//! via [`SdoClient::on_frame`], timeouts are detected in [`SdoClient::poll`].
//!
//! Supports expedited and segmented transfers (block transfer is on the
//! roadmap). Segmented data is staged in an internal buffer of `BUF` bytes
//! (default 256); transfers exceeding it fail cleanly.

use crate::cob;
use crate::sdo::SdoAbortCode;
use crate::{CanFrame, Micros, NodeId, TxSink};

/// Default SDO response timeout (500 ms, the customary CANopenNode value).
/// The timeout is per response, not per transfer, matching the C stack.
pub const DEFAULT_SDO_TIMEOUT_US: u64 = 500_000;

/// Errors when initiating a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdoClientError {
    /// A transfer is already in progress on this client.
    Busy,
    /// Download data must be at least 1 byte and fit the client buffer.
    InvalidDataLength,
}

/// Why a running transfer failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdoTransferError {
    /// The server aborted the transfer.
    Abort(SdoAbortCode),
    /// No response within the configured timeout (an abort was sent).
    Timeout,
    /// The uploaded value does not fit the client buffer (an abort was sent).
    BufferTooSmall,
    /// The response was malformed or did not match the request.
    Protocol,
}

/// Completion event of an SDO transfer. `UploadOk` borrows the client's
/// internal buffer: consume (or copy) the data before starting the next
/// transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdoEvent<'a> {
    /// Upload (read) finished; `data` is the value in little-endian byte
    /// order as defined by CiA 301.
    UploadOk {
        /// Object index.
        index: u16,
        /// Object sub-index.
        sub: u8,
        /// Value bytes (little-endian).
        data: &'a [u8],
    },
    /// Download (write) finished.
    DownloadOk {
        /// Object index.
        index: u16,
        /// Object sub-index.
        sub: u8,
    },
    /// The transfer failed.
    Failed {
        /// Object index.
        index: u16,
        /// Object sub-index.
        sub: u8,
        /// Failure reason.
        error: SdoTransferError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    AwaitUploadInitiate,
    AwaitUploadSegment {
        toggle: bool,
        received: usize,
        indicated: Option<u32>,
    },
    AwaitDownloadInitiate {
        len: usize,
    },
    AwaitDownloadSegment {
        toggle: bool,
        len: usize,
        sent: usize,
    },
}

/// An SDO client talking to one server node via the default SDO channel
/// (requests on 0x600 + server id, responses on 0x580 + server id).
///
/// One client handles one transfer at a time, matching the C implementation;
/// use one `SdoClient` per server node for concurrent parameterization.
#[derive(Debug)]
pub struct SdoClient<const BUF: usize = 256> {
    server: NodeId,
    timeout_us: u64,
    state: State,
    index: u16,
    sub: u8,
    since: Micros,
    buf: [u8; BUF],
}

impl<const BUF: usize> SdoClient<BUF> {
    /// Create a client for the given server node with the default timeout.
    pub fn new(server: NodeId) -> Self {
        Self {
            server,
            timeout_us: DEFAULT_SDO_TIMEOUT_US,
            state: State::Idle,
            index: 0,
            sub: 0,
            since: 0,
            buf: [0; BUF],
        }
    }

    /// Change the per-response timeout.
    pub fn set_timeout_us(&mut self, timeout_us: u64) {
        self.timeout_us = timeout_us;
    }

    /// COB-ID this client listens on (server responses), for RX filters.
    pub fn response_cob_id(&self) -> u16 {
        cob::sdo_tx(self.server)
    }

    /// Whether a transfer is currently in progress.
    pub fn is_busy(&self) -> bool {
        self.state != State::Idle
    }

    /// Start an upload (read `index:sub` from the server). Returns the
    /// request frame to transmit. Values up to 4 bytes arrive expedited,
    /// larger ones via segmented transfer into the client buffer.
    pub fn upload(&mut self, index: u16, sub: u8, now: Micros) -> Result<CanFrame, SdoClientError> {
        if self.is_busy() {
            return Err(SdoClientError::Busy);
        }
        self.begin(index, sub, now, State::AwaitUploadInitiate);
        Ok(self.request_frame(0x40, &[]))
    }

    /// Start a download (write `data` to `index:sub` on the server); `data`
    /// is the value in little-endian byte order. Up to 4 bytes are sent
    /// expedited, larger values (up to `BUF` bytes) via segmented transfer.
    /// Returns the request frame to transmit.
    pub fn download(
        &mut self,
        index: u16,
        sub: u8,
        data: &[u8],
        now: Micros,
    ) -> Result<CanFrame, SdoClientError> {
        if self.is_busy() {
            return Err(SdoClientError::Busy);
        }
        let len = data.len();
        if len == 0 || len > BUF {
            return Err(SdoClientError::InvalidDataLength);
        }
        if len <= 4 {
            // Expedited: ccs=1, e=1, s=1, n = number of unused bytes.
            let cmd = 0x23 | (((4 - len) as u8) << 2);
            self.begin(index, sub, now, State::AwaitDownloadInitiate { len });
            Ok(self.request_frame(cmd, data))
        } else {
            // Segmented initiate: ccs=1, e=0, s=1, size in bytes 4..8.
            self.buf[..len].copy_from_slice(data);
            self.begin(index, sub, now, State::AwaitDownloadInitiate { len });
            Ok(self.request_frame(0x21, &(len as u32).to_le_bytes()))
        }
    }

    /// Feed a received frame to the client. Frames not addressed to this
    /// client's response COB-ID are ignored. Emits at most one completion
    /// event; transmits follow-up segment requests and aborts via `tx`.
    pub fn on_frame(&mut self, frame: &CanFrame, now: Micros, tx: &mut impl TxSink) -> Option<SdoEvent<'_>> {
        if frame.id() != self.response_cob_id() || frame.dlc() != 8 || self.state == State::Idle {
            return None;
        }
        let data: [u8; 8] = frame.data().try_into().unwrap();
        self.since = now;

        // Abort from server ends any transfer.
        if data[0] == 0x80 {
            self.state = State::Idle;
            let code = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            return Some(SdoEvent::Failed {
                index: self.index,
                sub: self.sub,
                error: SdoTransferError::Abort(SdoAbortCode(code)),
            });
        }

        match self.state {
            State::AwaitUploadInitiate => self.on_upload_initiate(data, tx),
            State::AwaitUploadSegment { .. } => self.on_upload_segment(data, tx),
            State::AwaitDownloadInitiate { .. } => self.on_download_initiate(data, tx),
            State::AwaitDownloadSegment { .. } => self.on_download_segment(data, tx),
            State::Idle => unreachable!(),
        }
    }

    fn on_upload_initiate(&mut self, data: [u8; 8], tx: &mut impl TxSink) -> Option<SdoEvent<'_>> {
        if data[0] >> 5 != 2 || !self.object_matches(&data) {
            return Some(self.fail_with_abort(SdoAbortCode::CMD_SPECIFIER, SdoTransferError::Protocol, tx));
        }
        let expedited = data[0] & 0x02 != 0;
        let size_indicated = data[0] & 0x01 != 0;
        if expedited {
            let unused = if size_indicated { (data[0] >> 2) & 0x03 } else { 0 };
            let len = 4 - unused as usize;
            self.buf[..len].copy_from_slice(&data[4..4 + len]);
            self.state = State::Idle;
            return Some(SdoEvent::UploadOk {
                index: self.index,
                sub: self.sub,
                data: &self.buf[..len],
            });
        }
        // Segmented: size announcement in bytes 4..8 when indicated.
        let indicated = size_indicated.then(|| u32::from_le_bytes([data[4], data[5], data[6], data[7]]));
        if let Some(size) = indicated {
            if size as usize > BUF {
                return Some(self.fail_with_abort(
                    SdoAbortCode::OUT_OF_MEMORY,
                    SdoTransferError::BufferTooSmall,
                    tx,
                ));
            }
        }
        self.state = State::AwaitUploadSegment { toggle: false, received: 0, indicated };
        tx.send(self.request_frame(0x60, &[])); // first segment request, toggle 0
        None
    }

    fn on_upload_segment(&mut self, data: [u8; 8], tx: &mut impl TxSink) -> Option<SdoEvent<'_>> {
        let State::AwaitUploadSegment { toggle, received, indicated } = self.state else {
            unreachable!()
        };
        // Segment response: scs=0, toggle must echo our request.
        if data[0] >> 5 != 0 {
            return Some(self.fail_with_abort(SdoAbortCode::CMD_SPECIFIER, SdoTransferError::Protocol, tx));
        }
        if (data[0] >> 4) & 1 != toggle as u8 {
            return Some(self.fail_with_abort(SdoAbortCode::TOGGLE_BIT, SdoTransferError::Protocol, tx));
        }
        let unused = ((data[0] >> 1) & 0x07) as usize;
        let chunk = 7 - unused;
        let last = data[0] & 0x01 != 0;
        if received + chunk > BUF {
            return Some(self.fail_with_abort(
                SdoAbortCode::OUT_OF_MEMORY,
                SdoTransferError::BufferTooSmall,
                tx,
            ));
        }
        self.buf[received..received + chunk].copy_from_slice(&data[1..1 + chunk]);
        let received = received + chunk;

        if last {
            if indicated.is_some_and(|size| size as usize != received) {
                return Some(self.fail_with_abort(
                    SdoAbortCode::TYPE_MISMATCH,
                    SdoTransferError::Protocol,
                    tx,
                ));
            }
            self.state = State::Idle;
            return Some(SdoEvent::UploadOk {
                index: self.index,
                sub: self.sub,
                data: &self.buf[..received],
            });
        }
        let toggle = !toggle;
        self.state = State::AwaitUploadSegment { toggle, received, indicated };
        tx.send(self.request_frame(0x60 | (u8::from(toggle) << 4), &[]));
        None
    }

    fn on_download_initiate(&mut self, data: [u8; 8], tx: &mut impl TxSink) -> Option<SdoEvent<'_>> {
        let State::AwaitDownloadInitiate { len } = self.state else {
            unreachable!()
        };
        // Download initiate response: scs=3, object address echoed.
        if data[0] >> 5 != 3 || !self.object_matches(&data) {
            return Some(self.fail_with_abort(SdoAbortCode::CMD_SPECIFIER, SdoTransferError::Protocol, tx));
        }
        if len <= 4 {
            self.state = State::Idle;
            return Some(SdoEvent::DownloadOk { index: self.index, sub: self.sub });
        }
        self.state = State::AwaitDownloadSegment { toggle: false, len, sent: 0 };
        self.send_download_segment(tx);
        None
    }

    fn on_download_segment(&mut self, data: [u8; 8], tx: &mut impl TxSink) -> Option<SdoEvent<'_>> {
        let State::AwaitDownloadSegment { toggle, len, sent } = self.state else {
            unreachable!()
        };
        // Segment response: scs=1, toggle echoed.
        if data[0] >> 5 != 1 {
            return Some(self.fail_with_abort(SdoAbortCode::CMD_SPECIFIER, SdoTransferError::Protocol, tx));
        }
        if (data[0] >> 4) & 1 != toggle as u8 {
            return Some(self.fail_with_abort(SdoAbortCode::TOGGLE_BIT, SdoTransferError::Protocol, tx));
        }
        if sent >= len {
            self.state = State::Idle;
            return Some(SdoEvent::DownloadOk { index: self.index, sub: self.sub });
        }
        self.state = State::AwaitDownloadSegment { toggle: !toggle, len, sent };
        self.send_download_segment(tx);
        None
    }

    /// Send the next download segment and advance `sent`.
    fn send_download_segment(&mut self, tx: &mut impl TxSink) {
        let State::AwaitDownloadSegment { toggle, len, sent } = self.state else {
            unreachable!()
        };
        let chunk = (len - sent).min(7);
        let last = sent + chunk == len;
        let cmd = (u8::from(toggle) << 4) | (((7 - chunk) as u8) << 1) | u8::from(last);
        let mut payload = [0u8; 8];
        payload[0] = cmd;
        payload[1..1 + chunk].copy_from_slice(&self.buf[sent..sent + chunk]);
        tx.send(CanFrame::new(cob::sdo_rx(self.server), &payload).unwrap());
        self.state = State::AwaitDownloadSegment { toggle, len, sent: sent + chunk };
    }

    /// Check for a response timeout. Call periodically or at
    /// [`next_deadline`](Self::next_deadline). On timeout an abort frame is
    /// transmitted and a failure event returned.
    pub fn poll(&mut self, now: Micros, tx: &mut impl TxSink) -> Option<SdoEvent<'_>> {
        if self.state == State::Idle {
            return None;
        }
        if now.saturating_sub(self.since) >= self.timeout_us {
            Some(self.fail_with_abort(SdoAbortCode::TIMEOUT, SdoTransferError::Timeout, tx))
        } else {
            None
        }
    }

    /// The next deadline at which [`poll`](Self::poll) should run, if a
    /// transfer is in progress.
    pub fn next_deadline(&self) -> Option<Micros> {
        (self.state != State::Idle).then(|| self.since + self.timeout_us)
    }

    fn begin(&mut self, index: u16, sub: u8, now: Micros, state: State) {
        self.index = index;
        self.sub = sub;
        self.since = now;
        self.state = state;
    }

    /// Response frames echo the object address of the request in bytes 1..4.
    fn object_matches(&self, data: &[u8; 8]) -> bool {
        u16::from_le_bytes([data[1], data[2]]) == self.index && data[3] == self.sub
    }

    fn fail_with_abort(
        &mut self,
        code: SdoAbortCode,
        error: SdoTransferError,
        tx: &mut impl TxSink,
    ) -> SdoEvent<'static> {
        self.state = State::Idle;
        tx.send(self.request_frame(0x80, &code.0.to_le_bytes()));
        SdoEvent::Failed { index: self.index, sub: self.sub, error }
    }

    /// Build a client request frame addressing the current object:
    /// `[cmd, index lo, index hi, sub, d0..d3]`, always 8 bytes.
    fn request_frame(&self, cmd: u8, data: &[u8]) -> CanFrame {
        let mut payload = [0u8; 8];
        payload[0] = cmd;
        payload[1..3].copy_from_slice(&self.index.to_le_bytes());
        payload[3] = self.sub;
        payload[4..4 + data.len()].copy_from_slice(data);
        CanFrame::new(cob::sdo_rx(self.server), &payload).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: u8 = 0x22;
    const RX: u16 = 0x600 + SERVER as u16; // client -> server
    const TX: u16 = 0x580 + SERVER as u16; // server -> client

    fn client() -> SdoClient {
        SdoClient::new(NodeId::new(SERVER).unwrap())
    }

    fn response(payload: [u8; 8]) -> CanFrame {
        CanFrame::new(TX, &payload).unwrap()
    }

    #[test]
    fn upload_request_and_expedited_response() {
        let mut c = client();
        let req = c.upload(0x1017, 0x00, 0).unwrap();
        assert_eq!(req.id(), RX);
        assert_eq!(req.data(), &[0x40, 0x17, 0x10, 0x00, 0, 0, 0, 0]);
        assert!(c.is_busy());

        let mut sent = Vec::new();
        let ev = c.on_frame(&response([0x4B, 0x17, 0x10, 0x00, 0xE8, 0x03, 0, 0]), 1, &mut |f| {
            sent.push(f)
        });
        assert_eq!(
            ev,
            Some(SdoEvent::UploadOk { index: 0x1017, sub: 0, data: &[0xE8, 0x03] })
        );
        assert!(sent.is_empty());
        assert!(!c.is_busy());
    }

    #[test]
    fn expedited_download_roundtrip() {
        let mut c = client();
        let req = c.download(0x1017, 0x00, &500u16.to_le_bytes(), 0).unwrap();
        assert_eq!(req.data(), &[0x2B, 0x17, 0x10, 0x00, 0xF4, 0x01, 0, 0]);
        let ev = c.on_frame(&response([0x60, 0x17, 0x10, 0x00, 0, 0, 0, 0]), 1, &mut |_f| {});
        assert_eq!(ev, Some(SdoEvent::DownloadOk { index: 0x1017, sub: 0 }));
    }

    #[test]
    fn segmented_upload_two_segments() {
        let mut c = client();
        c.upload(0x1008, 0, 0).unwrap();

        // Initiate response: segmented, size = 10.
        let mut sent = Vec::new();
        let ev = c.on_frame(&response([0x41, 0x08, 0x10, 0x00, 10, 0, 0, 0]), 1, &mut |f| {
            sent.push(f)
        });
        assert_eq!(ev, None);
        // First segment request, toggle 0.
        assert_eq!(sent.last().unwrap().data(), &[0x60, 0x08, 0x10, 0x00, 0, 0, 0, 0]);

        // Segment 1: toggle 0, 7 bytes, not last.
        let ev = c.on_frame(
            &response([0x00, b'H', b'e', b'l', b'l', b'o', b' ', b'C']),
            2,
            &mut |f| sent.push(f),
        );
        assert_eq!(ev, None);
        // Second segment request, toggle 1.
        assert_eq!(sent.last().unwrap().data()[0], 0x70);

        // Segment 2: toggle 1, 3 bytes (n=4), last.
        let ev = c.on_frame(
            &response([0x19, b'A', b'N', b'!', 0, 0, 0, 0]),
            3,
            &mut |f| sent.push(f),
        );
        assert_eq!(
            ev,
            Some(SdoEvent::UploadOk { index: 0x1008, sub: 0, data: b"Hello CAN!" })
        );
        assert!(!c.is_busy());
    }

    #[test]
    fn segmented_upload_toggle_error_aborts() {
        let mut c = client();
        c.upload(0x1008, 0, 0).unwrap();
        c.on_frame(&response([0x41, 0x08, 0x10, 0x00, 10, 0, 0, 0]), 1, &mut |_f| {});

        // Segment arrives with wrong toggle (1 instead of 0).
        let mut sent = Vec::new();
        let ev = c.on_frame(&response([0x10, 1, 2, 3, 4, 5, 6, 7]), 2, &mut |f| sent.push(f));
        assert_eq!(
            ev,
            Some(SdoEvent::Failed {
                index: 0x1008,
                sub: 0,
                error: SdoTransferError::Protocol
            })
        );
        // Abort with toggle-bit code was sent.
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].data()[0], 0x80);
        assert_eq!(
            u32::from_le_bytes(sent[0].data()[4..8].try_into().unwrap()),
            SdoAbortCode::TOGGLE_BIT.0
        );
    }

    #[test]
    fn segmented_upload_size_mismatch_is_protocol_error() {
        let mut c = client();
        c.upload(0x1008, 0, 0).unwrap();
        c.on_frame(&response([0x41, 0x08, 0x10, 0x00, 9, 0, 0, 0]), 1, &mut |_f| {});
        // Last segment with 7 bytes but size said 9.
        let ev = c.on_frame(&response([0x01, 1, 2, 3, 4, 5, 6, 7]), 2, &mut |_f| {});
        assert_eq!(
            ev,
            Some(SdoEvent::Failed {
                index: 0x1008,
                sub: 0,
                error: SdoTransferError::Protocol
            })
        );
    }

    #[test]
    fn upload_exceeding_buffer_aborts_out_of_memory() {
        let mut c: SdoClient<8> = SdoClient::new(NodeId::new(SERVER).unwrap());
        c.upload(0x1008, 0, 0).unwrap();
        let mut sent = Vec::new();
        let ev = c.on_frame(&response([0x41, 0x08, 0x10, 0x00, 100, 0, 0, 0]), 1, &mut |f| {
            sent.push(f)
        });
        assert_eq!(
            ev,
            Some(SdoEvent::Failed {
                index: 0x1008,
                sub: 0,
                error: SdoTransferError::BufferTooSmall
            })
        );
        assert_eq!(
            u32::from_le_bytes(sent[0].data()[4..8].try_into().unwrap()),
            SdoAbortCode::OUT_OF_MEMORY.0
        );
    }

    #[test]
    fn segmented_download_sequence() {
        let mut c = client();
        let data = b"0123456789"; // 10 bytes -> 7 + 3
        let req = c.download(0x1008, 0, data, 0).unwrap();
        // Initiate: ccs=1, e=0, s=1, size 10.
        assert_eq!(req.data(), &[0x21, 0x08, 0x10, 0x00, 10, 0, 0, 0]);

        // Server accepts: 0x60 -> client sends segment 1 (toggle 0, 7 bytes).
        let mut sent = Vec::new();
        let ev = c.on_frame(&response([0x60, 0x08, 0x10, 0x00, 0, 0, 0, 0]), 1, &mut |f| {
            sent.push(f)
        });
        assert_eq!(ev, None);
        assert_eq!(sent.last().unwrap().data(), &[0x00, b'0', b'1', b'2', b'3', b'4', b'5', b'6']);

        // Server acks toggle 0 -> segment 2: toggle 1, 3 bytes (n=4), last.
        let ev = c.on_frame(&response([0x20, 0, 0, 0, 0, 0, 0, 0]), 2, &mut |f| sent.push(f));
        assert_eq!(ev, None);
        assert_eq!(sent.last().unwrap().data(), &[0x19, b'7', b'8', b'9', 0, 0, 0, 0]);

        // Server acks toggle 1 -> done.
        let ev = c.on_frame(&response([0x30, 0, 0, 0, 0, 0, 0, 0]), 3, &mut |f| sent.push(f));
        assert_eq!(ev, Some(SdoEvent::DownloadOk { index: 0x1008, sub: 0 }));
    }

    #[test]
    fn server_abort_is_reported() {
        let mut c = client();
        c.upload(0x1234, 0x05, 0).unwrap();
        let ev = c.on_frame(
            &response([0x80, 0x34, 0x12, 0x05, 0x00, 0x00, 0x02, 0x06]),
            1,
            &mut |_f| {},
        );
        assert_eq!(
            ev,
            Some(SdoEvent::Failed {
                index: 0x1234,
                sub: 5,
                error: SdoTransferError::Abort(SdoAbortCode::NO_OBJECT)
            })
        );
    }

    #[test]
    fn timeout_sends_abort_and_reports() {
        let mut c = client();
        c.upload(0x1017, 0, 1_000).unwrap();
        assert_eq!(c.next_deadline(), Some(1_000 + DEFAULT_SDO_TIMEOUT_US));

        let mut sent = Vec::new();
        assert_eq!(c.poll(400_000, &mut |f| sent.push(f)), None);
        let ev = c.poll(501_000, &mut |f| sent.push(f));
        assert_eq!(
            ev,
            Some(SdoEvent::Failed {
                index: 0x1017,
                sub: 0,
                error: SdoTransferError::Timeout
            })
        );
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].id(), RX);
        assert_eq!(sent[0].data(), &[0x80, 0x17, 0x10, 0x00, 0x00, 0x00, 0x04, 0x05]);
        assert!(!c.is_busy());
    }

    #[test]
    fn per_response_timeout_is_refreshed_by_segments() {
        let mut c = client();
        c.upload(0x1008, 0, 0).unwrap();
        c.on_frame(&response([0x41, 0x08, 0x10, 0x00, 10, 0, 0, 0]), 400_000, &mut |_f| {});
        // 500 ms after start but only 100 ms after the last response: alive.
        assert_eq!(c.poll(500_000, &mut |_f| {}), None);
        assert_eq!(c.next_deadline(), Some(900_000));
    }

    #[test]
    fn busy_and_invalid_length_are_rejected() {
        let mut c = client();
        c.upload(0x1000, 0, 0).unwrap();
        assert_eq!(c.upload(0x1001, 0, 0), Err(SdoClientError::Busy));

        let mut c2 = client();
        assert_eq!(c2.download(0x1001, 0, &[], 0), Err(SdoClientError::InvalidDataLength));
        let mut small: SdoClient<8> = SdoClient::new(NodeId::new(SERVER).unwrap());
        assert_eq!(
            small.download(0x1001, 0, &[0; 9], 0),
            Err(SdoClientError::InvalidDataLength)
        );
    }

    #[test]
    fn foreign_and_idle_frames_are_ignored() {
        let mut c = client();
        assert_eq!(c.on_frame(&response([0x60, 0, 0, 0, 0, 0, 0, 0]), 0, &mut |_f| {}), None);

        c.upload(0x1000, 0, 0).unwrap();
        let hb = CanFrame::new(0x700 + SERVER as u16, &[0x05]).unwrap();
        assert_eq!(c.on_frame(&hb, 1, &mut |_f| {}), None);
        let short = CanFrame::new(TX, &[0x60]).unwrap();
        assert_eq!(c.on_frame(&short, 2, &mut |_f| {}), None);
        assert!(c.is_busy());
    }

    #[test]
    fn mismatched_object_address_is_a_protocol_error() {
        let mut c = client();
        c.upload(0x1017, 0, 0).unwrap();
        let mut sent = Vec::new();
        let ev = c.on_frame(&response([0x4B, 0x18, 0x10, 0x00, 0, 0, 0, 0]), 1, &mut |f| {
            sent.push(f)
        });
        assert_eq!(
            ev,
            Some(SdoEvent::Failed {
                index: 0x1017,
                sub: 0,
                error: SdoTransferError::Protocol
            })
        );
        assert_eq!(sent.len(), 1);
    }
}
