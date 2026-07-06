//! End-to-end SDO transfers: the port's own client parameterizes the port's
//! own server, which serves the OD generated from `example/DS301_profile.json`
//! — the full round trip a master and a device perform on a real bus.

use canopen_core::sdo::{SdoAbortCode, SdoClient, SdoEvent, SdoServer, SdoTransferError};
use canopen_core::{CanFrame, NodeId};
use canopen_example_od::Od;

const SERVER_NODE: u8 = 0x0A;

/// Owned transfer outcome (the client's `UploadOk` borrows its buffer).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Upload(Vec<u8>),
    Download,
    Failed(SdoTransferError),
}

struct Bench {
    od: Od,
    server: SdoServer,
    client: SdoClient,
    now: u64,
}

impl Bench {
    fn new() -> Self {
        let id = NodeId::new(SERVER_NODE).unwrap();
        Self {
            od: Od::new(id),
            server: SdoServer::new(id),
            client: SdoClient::new(id),
            now: 0,
        }
    }

    /// Shuttle frames between client and server until the transfer
    /// completes — segmented transfers take several rounds.
    fn run(&mut self, request: CanFrame) -> Outcome {
        let mut to_server = vec![request];
        for _round in 0..100 {
            let mut to_client = Vec::new();
            for frame in to_server.drain(..) {
                self.now += 1;
                self.server
                    .on_frame(&frame, self.now, &mut self.od, &mut |f| to_client.push(f));
            }
            let mut next_to_server = Vec::new();
            for frame in &to_client {
                self.now += 1;
                match self.client.on_frame(frame, self.now, &mut |f| next_to_server.push(f)) {
                    Some(SdoEvent::UploadOk { data, .. }) => return Outcome::Upload(data.to_vec()),
                    Some(SdoEvent::DownloadOk { .. }) => return Outcome::Download,
                    Some(SdoEvent::Failed { error, .. }) => return Outcome::Failed(error),
                    None => {}
                }
            }
            to_server = next_to_server;
            if to_server.is_empty() {
                break;
            }
        }
        panic!("transfer did not complete");
    }

    fn read(&mut self, index: u16, sub: u8) -> Outcome {
        let req = self.client.upload(index, sub, self.now).unwrap();
        self.run(req)
    }

    fn write(&mut self, index: u16, sub: u8, data: &[u8]) -> Outcome {
        let req = self.client.download(index, sub, data, self.now).unwrap();
        self.run(req)
    }
}

#[test]
fn read_default_and_node_id_relative_values() {
    let mut b = Bench::new();
    // 0x1018:00 highest sub-index of Identity: 4.
    assert_eq!(b.read(0x1018, 0), Outcome::Upload(vec![4]));
    // 0x1200:02 SDO response COB-ID: $NODEID+0x580 resolved for node 0x0A.
    assert_eq!(b.read(0x1200, 2), Outcome::Upload(vec![0x8A, 0x05, 0, 0]));
}

#[test]
fn parameterize_heartbeat_time() {
    let mut b = Bench::new();
    assert_eq!(b.write(0x1017, 0, &500u16.to_le_bytes()), Outcome::Download);
    assert_eq!(b.od.x1017_producer_heartbeat_time, 500);
    assert_eq!(b.read(0x1017, 0), Outcome::Upload(vec![0xF4, 0x01]));
}

#[test]
fn server_aborts_are_reported_by_the_client() {
    let mut b = Bench::new();
    // Identity vendor id (0x1018:01) is read-only.
    assert_eq!(
        b.write(0x1018, 1, &1u32.to_le_bytes()),
        Outcome::Failed(SdoTransferError::Abort(SdoAbortCode::READ_ONLY))
    );
    // Nonexistent object.
    assert_eq!(
        b.read(0x6000, 0),
        Outcome::Failed(SdoTransferError::Abort(SdoAbortCode::NO_OBJECT))
    );
    // Wrong length for a u16 entry.
    assert_eq!(
        b.write(0x1017, 0, &500u32.to_le_bytes()),
        Outcome::Failed(SdoTransferError::Abort(SdoAbortCode::TYPE_MISMATCH))
    );
}

#[test]
fn segmented_download_to_scalar_entry_is_rejected_with_exact_length() {
    let mut b = Bench::new();
    // 10 bytes to a u16 entry: server accepts the transfer (capacity-wise),
    // the OD write then rejects the length.
    assert_eq!(
        b.write(0x1017, 0, b"0123456789"),
        Outcome::Failed(SdoTransferError::Abort(SdoAbortCode::TYPE_MISMATCH))
    );
    assert_eq!(b.od.x1017_producer_heartbeat_time, 0, "unchanged default");
}
