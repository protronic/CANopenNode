//! Heartbeat producer (CiA 301 §7.2.8.3.2), part of the port of
//! `301/CO_NMT_Heartbeat.*`.

use crate::cob;
use crate::nmt::NmtState;
use crate::{CanFrame, Micros, NodeId, TxSink};

/// Produces the boot-up message and cyclic heartbeat frames for one node.
///
/// The producer period comes from OD entry 0x1017 (producer heartbeat time,
/// u16 milliseconds); 0 disables the heartbeat.
#[derive(Debug)]
pub struct HeartbeatProducer {
    node_id: NodeId,
    period_us: u64,
    last_sent: Option<Micros>,
}

impl HeartbeatProducer {
    /// Create a producer with the given period in milliseconds (OD 0x1017).
    pub fn new(node_id: NodeId, period_ms: u16) -> Self {
        Self {
            node_id,
            period_us: u64::from(period_ms) * 1000,
            last_sent: None,
        }
    }

    /// Change the producer period at runtime (SDO write to 0x1017).
    /// Takes effect immediately; the next heartbeat is scheduled relative to
    /// the last one sent.
    pub fn set_period_ms(&mut self, period_ms: u16) {
        self.period_us = u64::from(period_ms) * 1000;
    }

    /// Emit the boot-up message (heartbeat COB-ID with state 0x00) and start
    /// the heartbeat cycle.
    pub fn send_boot_up(&mut self, now: Micros, tx: &mut impl TxSink) {
        let frame = CanFrame::new(cob::heartbeat(self.node_id), &[0x00]).unwrap();
        tx.send(frame);
        self.last_sent = Some(now);
    }

    /// Produce a heartbeat if the period has elapsed. Returns the next
    /// deadline, or `None` when the producer is disabled.
    pub fn process(&mut self, state: NmtState, now: Micros, tx: &mut impl TxSink) -> Option<Micros> {
        if self.period_us == 0 {
            return None;
        }
        let due = match self.last_sent {
            None => true,
            Some(last) => now.saturating_sub(last) >= self.period_us,
        };
        if due {
            let frame = CanFrame::new(cob::heartbeat(self.node_id), &[state as u8]).unwrap();
            tx.send(frame);
            self.last_sent = Some(now);
        }
        Some(self.last_sent.unwrap_or(now) + self.period_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn producer(period_ms: u16) -> HeartbeatProducer {
        HeartbeatProducer::new(NodeId::new(5).unwrap(), period_ms)
    }

    #[test]
    fn boot_up_message() {
        let mut hb = producer(1000);
        let mut sent = Vec::new();
        hb.send_boot_up(0, &mut |f| sent.push(f));
        assert_eq!(sent, [CanFrame::new(0x705, &[0x00]).unwrap()]);
    }

    #[test]
    fn cyclic_heartbeat_with_injected_clock() {
        let mut hb = producer(100); // 100 ms
        let mut sent = Vec::new();
        hb.send_boot_up(0, &mut |f| sent.push(f));

        // Not yet due at 50 ms.
        let next = hb.process(NmtState::PreOperational, 50_000, &mut |f| sent.push(f));
        assert_eq!(next, Some(100_000));
        assert_eq!(sent.len(), 1);

        // Due at 100 ms; pre-operational is reported as 0x7F.
        hb.process(NmtState::PreOperational, 100_000, &mut |f| sent.push(f));
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1], CanFrame::new(0x705, &[0x7F]).unwrap());

        // State changes show up in the next heartbeat.
        hb.process(NmtState::Operational, 200_000, &mut |f| sent.push(f));
        assert_eq!(sent[2], CanFrame::new(0x705, &[0x05]).unwrap());
    }

    #[test]
    fn disabled_when_period_zero() {
        let mut hb = producer(0);
        let mut count = 0usize;
        assert_eq!(
            hb.process(NmtState::Operational, 1_000_000, &mut |_f| count += 1),
            None
        );
        assert_eq!(count, 0);
    }

    #[test]
    fn period_change_at_runtime() {
        let mut hb = producer(0);
        let mut count = 0usize;
        hb.set_period_ms(10);
        hb.process(NmtState::Operational, 0, &mut |_f| count += 1);
        assert_eq!(count, 1); // first process after enabling sends immediately
        hb.process(NmtState::Operational, 5_000, &mut |_f| count += 1);
        assert_eq!(count, 1);
        hb.process(NmtState::Operational, 10_000, &mut |_f| count += 1);
        assert_eq!(count, 2);
    }
}
