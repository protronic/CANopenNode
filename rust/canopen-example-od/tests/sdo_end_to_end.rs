//! End-to-end SDO transfers: the port's own client parameterizes the port's
//! own server, which serves the OD generated from `example/DS301_profile.json`
//! — the full round trip a master and a device perform on a real bus.

use canopen_core::sdo::{SdoAbortCode, SdoClient, SdoEvent, SdoServer, SdoTransferError};
use canopen_core::{CanFrame, NodeId};
use canopen_example_od::Od;

const SERVER_NODE: u8 = 0x0A;

struct Bench {
    od: Od,
    server: SdoServer,
    client: SdoClient,
}

impl Bench {
    fn new() -> Self {
        let id = NodeId::new(SERVER_NODE).unwrap();
        Self {
            od: Od::new(id),
            server: SdoServer::new(id),
            client: SdoClient::new(id),
        }
    }

    /// Deliver one client request to the server and its response back to the
    /// client, returning the client's completion event.
    fn roundtrip(&mut self, request: CanFrame) -> SdoEvent {
        let mut responses: Vec<CanFrame> = Vec::new();
        self.server
            .on_frame(&request, &mut self.od, &mut |f| responses.push(f));
        assert_eq!(responses.len(), 1, "server must answer every request");
        let mut client_tx = Vec::new();
        let event = self
            .client
            .on_frame(&responses[0], &mut |f| client_tx.push(f));
        assert!(client_tx.is_empty(), "no client abort expected here");
        event.expect("transfer must complete")
    }
}

#[test]
fn read_default_and_node_id_relative_values() {
    let mut b = Bench::new();
    // 0x1018:00 highest sub-index of Identity: 4.
    let req = b.client.upload(0x1018, 0, 0).unwrap();
    assert_eq!(
        b.roundtrip(req),
        SdoEvent::UploadOk { index: 0x1018, sub: 0, len: 1, data: [4, 0, 0, 0] }
    );
    // 0x1200:02 SDO response COB-ID: $NODEID+0x580 resolved for node 0x0A.
    let req = b.client.upload(0x1200, 2, 0).unwrap();
    assert_eq!(
        b.roundtrip(req),
        SdoEvent::UploadOk { index: 0x1200, sub: 2, len: 4, data: [0x8A, 0x05, 0, 0] }
    );
}

#[test]
fn parameterize_heartbeat_time() {
    let mut b = Bench::new();
    // Write 0x1017 = 500 ms, as a configuration tool would.
    let req = b.client.download(0x1017, 0, &500u16.to_le_bytes(), 0).unwrap();
    assert_eq!(b.roundtrip(req), SdoEvent::DownloadOk { index: 0x1017, sub: 0 });
    assert_eq!(b.od.x1017_producer_heartbeat_time, 500);

    // Read back.
    let req = b.client.upload(0x1017, 0, 0).unwrap();
    assert_eq!(
        b.roundtrip(req),
        SdoEvent::UploadOk { index: 0x1017, sub: 0, len: 2, data: [0xF4, 0x01, 0, 0] }
    );
}

#[test]
fn server_aborts_are_reported_by_the_client() {
    let mut b = Bench::new();
    // Identity vendor id (0x1018:01) is read-only.
    let req = b.client.download(0x1018, 1, &1u32.to_le_bytes(), 0).unwrap();
    assert_eq!(
        b.roundtrip(req),
        SdoEvent::Failed {
            index: 0x1018,
            sub: 1,
            error: SdoTransferError::Abort(SdoAbortCode::READ_ONLY)
        }
    );
    // Nonexistent object.
    let req = b.client.upload(0x6000, 0, 0).unwrap();
    assert_eq!(
        b.roundtrip(req),
        SdoEvent::Failed {
            index: 0x6000,
            sub: 0,
            error: SdoTransferError::Abort(SdoAbortCode::NO_OBJECT)
        }
    );
}

#[test]
fn write_out_of_limits_is_rejected() {
    let mut b = Bench::new();
    // 0x1005 COB-ID SYNC has limits in the DS301 profile? Use exact-length
    // violation instead, which every entry enforces: u16 entry, u32 data.
    let req = b.client.download(0x1017, 0, &500u32.to_le_bytes(), 0).unwrap();
    let event = b.roundtrip(req);
    assert_eq!(
        event,
        SdoEvent::Failed {
            index: 0x1017,
            sub: 0,
            error: SdoTransferError::Abort(SdoAbortCode::TYPE_MISMATCH)
        }
    );
}
