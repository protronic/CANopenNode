//! PDO behavior over the OD generated from `example/DS301_profile.json`:
//! TPDO1 maps 0x2000:01 (1 bit), :02 (1 bit), :05 (8 bits); RPDO1 maps the
//! same shape of 0x2010 — the default mapping of the profile.

use canopen_core::{CanFrame, Node, NodeId};
use canopen_example_od::Od;

const NODE: u8 = 0x0A;

fn operational_node() -> (Node<Od>, Vec<CanFrame>) {
    let id = NodeId::new(NODE).unwrap();
    let mut node = Node::new(id, Od::new(id));
    let mut sent = Vec::new();
    node.start(0, &mut |f| sent.push(f));
    let start = CanFrame::new(0x000, &[0x01, NODE]).unwrap();
    node.on_frame(&start, 0, &mut |f| sent.push(f));
    sent.clear();
    (node, sent)
}

#[test]
fn tpdo1_sends_mapped_bits_on_request() {
    let (mut node, mut sent) = operational_node();
    node.od_mut().x2000_sub_object_1 = 1;
    node.od_mut().x2000_sub_object_5 = 0xA5;

    node.tpdo_request(0);
    node.process(1_000, &mut |f| sent.push(f));

    // COB-ID $NODEID+0x40000180: bit 30 (no RTR) masked -> 0x18A.
    // 10 mapped bits -> DLC 2; bit0 = sub1, bit1 = sub2, bits 2..10 = sub5.
    let pdo: Vec<&CanFrame> = sent.iter().filter(|f| f.id() == 0x18A).collect();
    assert_eq!(pdo.len(), 1);
    let expected = 0b1u64 | (0xA5u64 << 2);
    assert_eq!(pdo[0].data(), &expected.to_le_bytes()[..2]);
}

#[test]
fn sdo_write_to_mapped_object_triggers_tpdo() {
    let (mut node, mut sent) = operational_node();
    // SDO write 0x2000:05 = 0x42 (u8, expedited).
    let req = CanFrame::new(0x600 + NODE as u16, &[0x2F, 0x00, 0x20, 0x05, 0x42, 0, 0, 0]).unwrap();
    node.on_frame(&req, 1_000, &mut |f| sent.push(f));
    node.process(2_000, &mut |f| sent.push(f));

    let pdo: Vec<&CanFrame> = sent.iter().filter(|f| f.id() == 0x18A).collect();
    assert_eq!(pdo.len(), 1, "SDO write must trigger the event TPDO");
    let expected = 0x42u64 << 2;
    assert_eq!(pdo[0].data(), &expected.to_le_bytes()[..2]);
}

#[test]
fn rpdo1_updates_od_when_operational() {
    let (mut node, mut sent) = operational_node();
    let word = 0b11u64 | (0x37u64 << 2);
    let frame = CanFrame::new(0x20A, &word.to_le_bytes()[..2]).unwrap();
    node.on_frame(&frame, 1_000, &mut |f| sent.push(f));

    assert_eq!(node.od().x2010_sub_object_1, 1);
    assert_eq!(node.od().x2010_sub_object_2, 1);
    assert_eq!(node.od().x2010_sub_object_5, 0x37);
}

#[test]
fn pdos_are_inactive_outside_operational() {
    let id = NodeId::new(NODE).unwrap();
    let mut node = Node::new(id, Od::new(id));
    let mut sent = Vec::new();
    node.start(0, &mut |f| sent.push(f));
    sent.clear();

    // Pre-operational: RPDO ignored, TPDO request produces nothing.
    let word = 0b11u64;
    let frame = CanFrame::new(0x20A, &word.to_le_bytes()[..2]).unwrap();
    node.on_frame(&frame, 1_000, &mut |f| sent.push(f));
    assert_eq!(node.od().x2010_sub_object_1, 0);

    node.tpdo_request(0);
    node.process(2_000, &mut |f| sent.push(f));
    assert!(sent.iter().all(|f| f.id() != 0x18A));
}
