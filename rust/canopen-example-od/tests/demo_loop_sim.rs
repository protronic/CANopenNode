//! Simulation of the canopen-demo `node` main loop (regression test for the
//! "one PDO, then the node dies" report): replicates the exact loop
//! structure — process(now) -> recv(timeout from deadline) — with a virtual
//! bus and clock, and fails if the loop ever computes a zero timeout (a
//! zero SO_RCVTIMEO makes the real SocketCAN socket block forever) or stops
//! making progress.

use canopen_core::{CanFrame, Node, NodeId};
use canopen_example_od::Od;

const NODE: u8 = 10;

struct SimBus {
    now: u64,
    /// Frames arriving on the bus at absolute times (sorted).
    pending: Vec<(u64, CanFrame)>,
    /// Everything the node transmitted, with timestamps.
    sent: Vec<(u64, CanFrame)>,
    zero_timeouts: u32,
}

impl SimBus {
    /// Mirror of SocketCanBus::recv driven by virtual time.
    fn recv(&mut self, timeout_us: u64) -> Option<CanFrame> {
        if timeout_us == 0 {
            self.zero_timeouts += 1;
        }
        if let Some((at, frame)) = self.pending.first().copied() {
            if at <= self.now + timeout_us {
                self.now = self.now.max(at);
                self.pending.remove(0);
                return Some(frame);
            }
        }
        self.now += timeout_us;
        None
    }
}

/// Replica of the run_node inner loop from canopen-demo/src/main.rs.
fn simulate(event_timer_ms: u16, until_us: u64) -> SimBus {
    let node_id = NodeId::new(NODE).unwrap();
    let mut od = Od::new(node_id);
    od.x1017_producer_heartbeat_time = 1000;
    od.x1800_event_timer = event_timer_ms;

    let mut bus = SimBus {
        now: 0,
        pending: vec![
            // NMT start after 100 ms, as `canopen-demo nmt vcan0 start 10`.
            (100_000, CanFrame::new(0x000, &[0x01, NODE]).unwrap()),
        ],
        sent: Vec::new(),
        zero_timeouts: 0,
    };

    let mut node = Node::new(node_id, od.clone());
    let now0 = bus.now;
    let mut queued: Vec<CanFrame> = Vec::new();
    node.start(now0, &mut |f| queued.push(f));
    for f in queued.drain(..) {
        bus.sent.push((bus.now, f));
    }

    let mut iterations = 0u32;
    while bus.now < until_us {
        iterations += 1;
        assert!(iterations < 100_000, "loop stopped making progress (busy loop)");

        let now = bus.now;
        let mut queued: Vec<CanFrame> = Vec::new();
        let next = node.process(now, &mut |f| queued.push(f));
        for f in queued.drain(..) {
            bus.sent.push((bus.now, f));
        }
        // Exactly the demo's timeout computation.
        let timeout_us = next.map(|deadline| deadline.saturating_sub(now)).unwrap_or(100_000);

        if let Some(frame) = bus.recv(timeout_us) {
            let mut queued: Vec<CanFrame> = Vec::new();
            let reset = node.on_frame(&frame, bus.now, &mut |f| queued.push(f));
            for f in queued.drain(..) {
                bus.sent.push((bus.now, f));
            }
            assert!(reset.is_none(), "unexpected reset request");
        }
    }
    bus
}

#[test]
fn cyclic_tpdo_and_heartbeat_survive_five_seconds() {
    let bus = simulate(1000, 5_000_000);

    let pdos: Vec<u64> = bus.sent.iter().filter(|(_, f)| f.id() == 0x18A).map(|(t, _)| *t).collect();
    let heartbeats: Vec<u64> = bus.sent.iter().filter(|(_, f)| f.id() == 0x70A).map(|(t, _)| *t).collect();

    assert_eq!(bus.zero_timeouts, 0, "zero recv timeout blocks the real socket forever");
    // Event timer 1000 ms, operational from t=100ms: PDOs at ~1.1s, 2.1s, 3.1s, 4.1s.
    assert!(
        pdos.len() >= 4,
        "expected cyclic TPDOs, got {} at {:?}",
        pdos.len(),
        pdos
    );
    // Boot-up + heartbeat every second, the whole time.
    assert!(
        heartbeats.len() >= 5,
        "heartbeats must continue, got {} at {:?}",
        heartbeats.len(),
        heartbeats
    );
    assert!(
        *heartbeats.last().unwrap() >= 4_000_000,
        "heartbeats stopped early: last at {}",
        heartbeats.last().unwrap()
    );
}

#[test]
fn without_event_timer_no_pdo_but_alive() {
    let bus = simulate(0, 3_000_000);
    assert_eq!(bus.zero_timeouts, 0);
    let pdos = bus.sent.iter().filter(|(_, f)| f.id() == 0x18A).count();
    assert_eq!(pdos, 0, "no event timer, no request -> no TPDO");
    let heartbeats = bus.sent.iter().filter(|(_, f)| f.id() == 0x70A).count();
    assert!(heartbeats >= 3);
}
