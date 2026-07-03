//! SDO client (CiA 301 §7.2.4), port of `301/CO_SDOclient.*`.
//!
//! Sans-IO: requests return the frame to transmit, responses are pushed in
//! via [`SdoClient::on_frame`], timeouts are detected in [`SdoClient::poll`].

use crate::cob;
use crate::sdo::SdoAbortCode;
use crate::{CanFrame, Micros, NodeId, TxSink};

/// Default SDO response timeout (500 ms, the customary CANopenNode value).
pub const DEFAULT_SDO_TIMEOUT_US: u64 = 500_000;

/// Errors when initiating a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdoClientError {
    /// A transfer is already in progress on this client.
    Busy,
    /// Expedited download data must be 1..=4 bytes.
    InvalidDataLength,
}

/// Why a running transfer failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdoTransferError {
    /// The server aborted the transfer.
    Abort(SdoAbortCode),
    /// No response within the configured timeout (an abort was sent).
    Timeout,
    /// The server answered with a segmented transfer, which this client does
    /// not support yet; `size` is the announced data size if indicated.
    SegmentedUnsupported {
        /// Total transfer size announced by the server, if any.
        size: Option<u32>,
    },
    /// The response was malformed or did not match the request.
    Protocol,
}

/// Completion event of an SDO transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdoEvent {
    /// Expedited upload (read) finished; `data[..len]` holds the value in
    /// little-endian byte order as defined by CiA 301.
    UploadOk {
        /// Object index.
        index: u16,
        /// Object sub-index.
        sub: u8,
        /// Number of valid bytes in `data`.
        len: u8,
        /// Value bytes (little-endian), `data[..len as usize]` valid.
        data: [u8; 4],
    },
    /// Expedited download (write) finished.
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

#[derive(Debug, Clone, Copy)]
enum State {
    Idle,
    AwaitUpload {
        index: u16,
        sub: u8,
        since: Micros,
    },
    AwaitDownload {
        index: u16,
        sub: u8,
        since: Micros,
    },
}

/// An SDO client talking to one server node via the default SDO channel
/// (requests on 0x600 + server id, responses on 0x580 + server id).
///
/// One client handles one transfer at a time, matching the C implementation;
/// use one `SdoClient` per server node for concurrent parameterization.
#[derive(Debug)]
pub struct SdoClient {
    server: NodeId,
    timeout_us: u64,
    state: State,
}

impl SdoClient {
    /// Create a client for the given server node with the default timeout.
    pub fn new(server: NodeId) -> Self {
        Self {
            server,
            timeout_us: DEFAULT_SDO_TIMEOUT_US,
            state: State::Idle,
        }
    }

    /// Change the response timeout.
    pub fn set_timeout_us(&mut self, timeout_us: u64) {
        self.timeout_us = timeout_us;
    }

    /// COB-ID this client listens on (server responses), for RX filters.
    pub fn response_cob_id(&self) -> u16 {
        cob::sdo_tx(self.server)
    }

    /// Whether a transfer is currently in progress.
    pub fn is_busy(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// Start an expedited upload (read `index:sub` from the server).
    /// Returns the request frame to transmit.
    pub fn upload(&mut self, index: u16, sub: u8, now: Micros) -> Result<CanFrame, SdoClientError> {
        if self.is_busy() {
            return Err(SdoClientError::Busy);
        }
        self.state = State::AwaitUpload { index, sub, since: now };
        Ok(self.request_frame(0x40, index, sub, &[]))
    }

    /// Start an expedited download (write `data` to `index:sub` on the
    /// server); `data` is the value in little-endian byte order, 1..=4 bytes.
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
        if len == 0 || len > 4 {
            return Err(SdoClientError::InvalidDataLength);
        }
        // ccs=1, e=1, s=1, n = number of unused bytes: 0x23 | n << 2.
        let cmd = 0x23 | (((4 - len) as u8) << 2);
        self.state = State::AwaitDownload { index, sub, since: now };
        Ok(self.request_frame(cmd, index, sub, data))
    }

    /// Feed a received frame to the client. Frames not addressed to this
    /// client's response COB-ID are ignored. Emits at most one completion
    /// event; may transmit an abort frame via `tx` on protocol errors.
    pub fn on_frame(&mut self, frame: &CanFrame, tx: &mut impl TxSink) -> Option<SdoEvent> {
        if frame.id() != self.response_cob_id() || frame.dlc() != 8 {
            return None;
        }
        let data = frame.data();
        let scs = data[0] >> 5;
        let (index, sub) = match self.state {
            State::Idle => return None,
            State::AwaitUpload { index, sub, .. } | State::AwaitDownload { index, sub, .. } => {
                (index, sub)
            }
        };

        // Abort from server (cs = 0x80) ends any transfer.
        if data[0] == 0x80 {
            self.state = State::Idle;
            let code = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            return Some(SdoEvent::Failed {
                index,
                sub,
                error: SdoTransferError::Abort(SdoAbortCode(code)),
            });
        }

        // The response must echo the requested object address.
        let rx_index = u16::from_le_bytes([data[1], data[2]]);
        if rx_index != index || data[3] != sub {
            return Some(self.fail_with_abort(SdoAbortCode::CMD_SPECIFIER, SdoTransferError::Protocol, tx));
        }

        match (self.state, scs) {
            // Upload initiate response (scs = 2).
            (State::AwaitUpload { .. }, 2) => {
                let expedited = data[0] & 0x02 != 0;
                let size_indicated = data[0] & 0x01 != 0;
                if expedited {
                    let unused = if size_indicated { (data[0] >> 2) & 0x03 } else { 0 };
                    let len = 4 - unused;
                    self.state = State::Idle;
                    Some(SdoEvent::UploadOk {
                        index,
                        sub,
                        len,
                        data: [data[4], data[5], data[6], data[7]],
                    })
                } else {
                    // Segmented transfer: not supported yet, abort cleanly.
                    let size = size_indicated
                        .then(|| u32::from_le_bytes([data[4], data[5], data[6], data[7]]));
                    Some(self.fail_with_abort(
                        SdoAbortCode::CMD_SPECIFIER,
                        SdoTransferError::SegmentedUnsupported { size },
                        tx,
                    ))
                }
            }
            // Download initiate response (scs = 3).
            (State::AwaitDownload { .. }, 3) => {
                self.state = State::Idle;
                Some(SdoEvent::DownloadOk { index, sub })
            }
            _ => Some(self.fail_with_abort(SdoAbortCode::CMD_SPECIFIER, SdoTransferError::Protocol, tx)),
        }
    }

    /// Check for a response timeout. Call periodically (or at the deadline
    /// returned as `Some(since + timeout)` semantics by the caller's timer).
    /// On timeout an abort frame is transmitted and a failure event returned.
    pub fn poll(&mut self, now: Micros, tx: &mut impl TxSink) -> Option<SdoEvent> {
        let since = match self.state {
            State::Idle => return None,
            State::AwaitUpload { since, .. } | State::AwaitDownload { since, .. } => since,
        };
        if now.saturating_sub(since) >= self.timeout_us {
            Some(self.fail_with_abort(SdoAbortCode::TIMEOUT, SdoTransferError::Timeout, tx))
        } else {
            None
        }
    }

    /// The next deadline at which [`poll`](Self::poll) should run, if a
    /// transfer is in progress.
    pub fn next_deadline(&self) -> Option<Micros> {
        match self.state {
            State::Idle => None,
            State::AwaitUpload { since, .. } | State::AwaitDownload { since, .. } => {
                Some(since + self.timeout_us)
            }
        }
    }

    fn fail_with_abort(
        &mut self,
        code: SdoAbortCode,
        error: SdoTransferError,
        tx: &mut impl TxSink,
    ) -> SdoEvent {
        let (index, sub) = match self.state {
            State::Idle => (0, 0),
            State::AwaitUpload { index, sub, .. } | State::AwaitDownload { index, sub, .. } => {
                (index, sub)
            }
        };
        self.state = State::Idle;
        let code_bytes = code.0.to_le_bytes();
        tx.send(self.request_frame(0x80, index, sub, &code_bytes));
        SdoEvent::Failed { index, sub, error }
    }

    /// Build a client request frame: `[cmd, index lo, index hi, sub, d0..d3]`,
    /// always 8 bytes as required for SDO.
    fn request_frame(&self, cmd: u8, index: u16, sub: u8, data: &[u8]) -> CanFrame {
        let mut payload = [0u8; 8];
        payload[0] = cmd;
        payload[1..3].copy_from_slice(&index.to_le_bytes());
        payload[3] = sub;
        payload[4..4 + data.len()].copy_from_slice(data);
        CanFrame::new(cob::sdo_rx(self.server), &payload).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: u8 = 0x22;

    fn client() -> SdoClient {
        SdoClient::new(NodeId::new(SERVER).unwrap())
    }

    fn response(payload: [u8; 8]) -> CanFrame {
        CanFrame::new(0x580 + SERVER as u16, &payload).unwrap()
    }

    #[test]
    fn upload_request_and_expedited_response() {
        let mut c = client();
        let req = c.upload(0x1017, 0x00, 0).unwrap();
        assert_eq!(req.id(), 0x600 + SERVER as u16);
        assert_eq!(req.data(), &[0x40, 0x17, 0x10, 0x00, 0, 0, 0, 0]);
        assert!(c.is_busy());

        // Expedited response, 2 bytes (n = 2): value 1000 ms.
        let mut sent = Vec::new();
        let ev = c.on_frame(&response([0x4B, 0x17, 0x10, 0x00, 0xE8, 0x03, 0, 0]), &mut |f| {
            sent.push(f)
        });
        assert_eq!(
            ev,
            Some(SdoEvent::UploadOk {
                index: 0x1017,
                sub: 0,
                len: 2,
                data: [0xE8, 0x03, 0, 0]
            })
        );
        assert!(sent.is_empty());
        assert!(!c.is_busy());
    }

    #[test]
    fn upload_expedited_without_size_indication_yields_four_bytes() {
        let mut c = client();
        c.upload(0x1000, 0, 0).unwrap();
        // e=1, s=0 -> 0x42, length defaults to 4.
        let ev = c.on_frame(&response([0x42, 0x00, 0x10, 0x00, 1, 2, 3, 4]), &mut |_f| {});
        assert_eq!(
            ev,
            Some(SdoEvent::UploadOk {
                index: 0x1000,
                sub: 0,
                len: 4,
                data: [1, 2, 3, 4]
            })
        );
    }

    #[test]
    fn download_request_and_response() {
        let mut c = client();
        // Write heartbeat time 0x1017:00 = 500 ms (u16).
        let req = c.download(0x1017, 0x00, &500u16.to_le_bytes(), 0).unwrap();
        assert_eq!(req.data(), &[0x2B, 0x17, 0x10, 0x00, 0xF4, 0x01, 0, 0]);

        let ev = c.on_frame(&response([0x60, 0x17, 0x10, 0x00, 0, 0, 0, 0]), &mut |_f| {});
        assert_eq!(ev, Some(SdoEvent::DownloadOk { index: 0x1017, sub: 0 }));
        assert!(!c.is_busy());
    }

    #[test]
    fn download_command_bytes_for_all_lengths() {
        for (len, cmd) in [(1usize, 0x2Fu8), (2, 0x2B), (3, 0x27), (4, 0x23)] {
            let mut c = client();
            let req = c.download(0x2000, 1, &[0xAA; 4][..len], 0).unwrap();
            assert_eq!(req.data()[0], cmd, "len {len}");
        }
    }

    #[test]
    fn server_abort_is_reported() {
        let mut c = client();
        c.upload(0x1234, 0x05, 0).unwrap();
        let ev = c.on_frame(
            &response([0x80, 0x34, 0x12, 0x05, 0x00, 0x00, 0x02, 0x06]),
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
        // Abort frame: cs 0x80, object address, abort code 0x05040000 LE.
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].id(), 0x600 + SERVER as u16);
        assert_eq!(sent[0].data(), &[0x80, 0x17, 0x10, 0x00, 0x00, 0x00, 0x04, 0x05]);
        assert!(!c.is_busy());
    }

    #[test]
    fn segmented_response_aborts_cleanly() {
        let mut c = client();
        c.upload(0x1008, 0, 0).unwrap(); // device name: usually a string
        let mut sent = Vec::new();
        // scs=2, e=0, s=1, size = 16 bytes.
        let ev = c.on_frame(&response([0x41, 0x08, 0x10, 0x00, 16, 0, 0, 0]), &mut |f| {
            sent.push(f)
        });
        assert_eq!(
            ev,
            Some(SdoEvent::Failed {
                index: 0x1008,
                sub: 0,
                error: SdoTransferError::SegmentedUnsupported { size: Some(16) }
            })
        );
        assert_eq!(sent.len(), 1, "abort frame must be sent");
        assert_eq!(sent[0].data()[0], 0x80);
    }

    #[test]
    fn busy_and_invalid_length_are_rejected() {
        let mut c = client();
        c.upload(0x1000, 0, 0).unwrap();
        assert_eq!(c.upload(0x1001, 0, 0), Err(SdoClientError::Busy));
        assert_eq!(
            c.download(0x1001, 0, &[0, 0, 0, 0, 0], 0),
            Err(SdoClientError::Busy)
        );

        let mut c2 = client();
        assert_eq!(
            c2.download(0x1001, 0, &[], 0),
            Err(SdoClientError::InvalidDataLength)
        );
        assert_eq!(
            c2.download(0x1001, 0, &[0; 5], 0),
            Err(SdoClientError::InvalidDataLength)
        );
    }

    #[test]
    fn foreign_and_idle_frames_are_ignored() {
        let mut c = client();
        // Idle: nothing happens.
        assert_eq!(c.on_frame(&response([0x60, 0, 0, 0, 0, 0, 0, 0]), &mut |_f| {}), None);

        c.upload(0x1000, 0, 0).unwrap();
        // Wrong COB-ID (heartbeat of some node).
        let hb = CanFrame::new(0x700 + SERVER as u16, &[0x05]).unwrap();
        assert_eq!(c.on_frame(&hb, &mut |_f| {}), None);
        // Wrong DLC.
        let short = CanFrame::new(0x580 + SERVER as u16, &[0x60]).unwrap();
        assert_eq!(c.on_frame(&short, &mut |_f| {}), None);
        assert!(c.is_busy());
    }

    #[test]
    fn mismatched_object_address_is_a_protocol_error() {
        let mut c = client();
        c.upload(0x1017, 0, 0).unwrap();
        let mut sent = Vec::new();
        let ev = c.on_frame(&response([0x4B, 0x18, 0x10, 0x00, 0, 0, 0, 0]), &mut |f| {
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
