//! Segmented SDO end-to-end: this port's client against this port's server,
//! over an OD with a string entry — verifies that toggle bits, chunking and
//! size indication of both implementations actually interoperate.

use canopen_core::od::{
    self, DataType, EntryInfo, ObjectDictionary, OdError, OdString, PdoAccess, SdoAccess,
};
use canopen_core::sdo::{SdoClient, SdoEvent, SdoServer, SdoTransferError};
use canopen_core::{CanFrame, NodeId};

/// OD with one string entry 0x2000:00 (rw, capacity 64).
#[derive(Default)]
struct StringOd {
    label: OdString<64>,
}

impl ObjectDictionary for StringOd {
    fn info(&self, index: u16, sub: u8) -> Result<EntryInfo, OdError> {
        match (index, sub) {
            (0x2000, 0) => Ok(EntryInfo {
                data_type: DataType::VisibleString,
                sdo: SdoAccess::ReadWrite,
                pdo: PdoAccess::No,
                size: self.label.len(),
            }),
            (0x2000, _) => Err(OdError::SubIndexNotFound),
            _ => Err(OdError::ObjectNotFound),
        }
    }

    fn read(&self, index: u16, sub: u8, buf: &mut [u8]) -> Result<usize, OdError> {
        match (index, sub) {
            (0x2000, 0) => od::read_bytes(buf, self.label.as_bytes()),
            (0x2000, _) => Err(OdError::SubIndexNotFound),
            _ => Err(OdError::ObjectNotFound),
        }
    }

    fn write(&mut self, index: u16, sub: u8, data: &[u8]) -> Result<(), OdError> {
        match (index, sub) {
            (0x2000, 0) => self.label.set(data),
            (0x2000, _) => Err(OdError::SubIndexNotFound),
            _ => Err(OdError::ObjectNotFound),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Upload(Vec<u8>),
    Download,
    Failed(SdoTransferError),
}

fn run(client: &mut SdoClient, server: &mut SdoServer, od: &mut StringOd, request: CanFrame) -> Outcome {
    let mut now = 0u64;
    let mut to_server = vec![request];
    for _ in 0..100 {
        let mut to_client = Vec::new();
        for frame in to_server.drain(..) {
            now += 1;
            server.on_frame(&frame, now, od, &mut |f| to_client.push(f));
        }
        let mut next = Vec::new();
        for frame in &to_client {
            now += 1;
            match client.on_frame(frame, now, &mut |f| next.push(f)) {
                Some(SdoEvent::UploadOk { data, .. }) => return Outcome::Upload(data.to_vec()),
                Some(SdoEvent::DownloadOk { .. }) => return Outcome::Download,
                Some(SdoEvent::Failed { error, .. }) => return Outcome::Failed(error),
                None => {}
            }
        }
        to_server = next;
        if to_server.is_empty() {
            break;
        }
    }
    panic!("transfer did not complete");
}

#[test]
fn segmented_write_then_read_roundtrip() {
    let id = NodeId::new(5).unwrap();
    let mut od = StringOd::default();
    let mut server = SdoServer::new(id);
    let mut client = SdoClient::new(id);

    // 29 bytes: initiate + 5 segments each way.
    let text = b"CANopenNode Rust @ protronic!";
    let req = client.download(0x2000, 0, text, 0).unwrap();
    assert_eq!(run(&mut client, &mut server, &mut od, req), Outcome::Download);
    assert_eq!(od.label.as_bytes(), text);

    let req = client.upload(0x2000, 0, 0).unwrap();
    assert_eq!(
        run(&mut client, &mut server, &mut od, req),
        Outcome::Upload(text.to_vec())
    );
}

#[test]
fn exact_multiple_of_seven_roundtrip() {
    // 14 bytes: the last segment carries exactly 7 bytes; the extra empty
    // "last" flag handling must still terminate correctly.
    let id = NodeId::new(5).unwrap();
    let mut od = StringOd::default();
    let mut server = SdoServer::new(id);
    let mut client = SdoClient::new(id);

    let text = b"14bytes-exact!";
    assert_eq!(text.len(), 14);
    let req = client.download(0x2000, 0, text, 0).unwrap();
    assert_eq!(run(&mut client, &mut server, &mut od, req), Outcome::Download);
    assert_eq!(od.label.as_bytes(), text);

    let req = client.upload(0x2000, 0, 0).unwrap();
    assert_eq!(
        run(&mut client, &mut server, &mut od, req),
        Outcome::Upload(text.to_vec())
    );
}

#[test]
fn oversized_write_is_rejected_by_the_entry() {
    let id = NodeId::new(5).unwrap();
    let mut od = StringOd::default();
    let mut server = SdoServer::new(id);
    let mut client = SdoClient::new(id);

    // 70 bytes exceed OdString<64>: fits both buffers, rejected by the OD.
    let big = [b'x'; 70];
    let req = client.download(0x2000, 0, &big, 0).unwrap();
    let outcome = run(&mut client, &mut server, &mut od, req);
    assert_eq!(
        outcome,
        Outcome::Failed(SdoTransferError::Abort(
            canopen_core::sdo::SdoAbortCode::DATA_LONG
        ))
    );
    assert!(od.label.is_empty());
}
