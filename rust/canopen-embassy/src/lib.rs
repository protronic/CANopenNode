//! Embassy async runner for the CANopenNode Rust port.
//!
//! Drives a sans-IO [`canopen_core::Node`] from an async CAN bus: received
//! frames are fed into the node, the node's `timerNext_us` deadline hint is
//! turned into an [`embassy_time::Timer`], and outgoing frames are flushed
//! to the bus. This crate is chip-independent — it only needs `embassy-time`
//! (whose driver the end binary provides) and an implementation of
//! [`NodeBus`] for the actual CAN peripheral (embassy-stm32 bxCAN/FDCAN,
//! SocketCAN, ...).
//!
//! ```ignore
//! loop {
//!     let mut node = Node::new(node_id, Od::new(node_id));
//!     match canopen_embassy::run(&mut node, &mut bus).await {
//!         ResetCommand::Communication => continue, // fresh node, boot-up again
//!         ResetCommand::Node => cortex_m::peripheral::SCB::sys_reset(),
//!     }
//! }
//! ```

#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

use canopen_core::nmt::NmtState;
use canopen_core::od::ObjectDictionary;
use canopen_core::{CanFrame, Node, ResetCommand};
use embassy_futures::select::{select, Either};
use embassy_time::{Instant, Timer};

/// An async classic-CAN bus carrying CANopen frames.
///
/// Implementations wrap the real peripheral (e.g. embassy-stm32 `Can`) and
/// convert to/from [`CanFrame`] — `CanFrame::from_embedded`/`to_embedded`
/// cover every `embedded_can::Frame` implementor. `recv` should skip frames
/// CANopen never uses (extended ids, remote frames) and may handle bus
/// errors internally (retry / wait for recovery).
#[allow(async_fn_in_trait)] // static dispatch only, like embedded-hal-async
pub trait NodeBus {
    /// Receive the next CANopen-relevant frame.
    async fn recv(&mut self) -> CanFrame;
    /// Transmit one frame.
    async fn send(&mut self, frame: CanFrame);
}

/// Maximum frames a single `process`/`on_frame` call may emit before the
/// queue is flushed to the bus.
const TX_QUEUE: usize = 16;

/// Drive one node until it requests a reset.
///
/// Sends the boot-up message if the node is freshly created, then loops:
/// wait for either a received frame or the node's next deadline, feed the
/// node, flush its output. Returns when an NMT reset command arrives — the
/// caller recreates the node (communication reset) or resets the device.
pub async fn run<OD: ObjectDictionary>(
    node: &mut Node<OD>,
    bus: &mut impl NodeBus,
) -> ResetCommand {
    let mut txq: heapless::Vec<CanFrame, TX_QUEUE> = heapless::Vec::new();

    if node.nmt_state() == NmtState::Initializing {
        node.start(now(), &mut queue_sink(&mut txq));
        flush(&mut txq, bus).await;
    }

    loop {
        let next = node.process(now(), &mut queue_sink(&mut txq));
        flush(&mut txq, bus).await;

        let received = match next {
            Some(deadline) => {
                match select(bus.recv(), Timer::at(Instant::from_micros(deadline))).await {
                    Either::First(frame) => Some(frame),
                    Either::Second(()) => None,
                }
            }
            None => Some(bus.recv().await),
        };

        if let Some(frame) = received {
            let reset = node.on_frame(&frame, now(), &mut queue_sink(&mut txq));
            flush(&mut txq, bus).await;
            if let Some(reset) = reset {
                return reset;
            }
        }
    }
}

fn now() -> canopen_core::Micros {
    Instant::now().as_micros()
}

/// Sink adapter: the sans-IO core emits synchronously into the queue, the
/// async loop flushes it afterwards. Overflow cannot happen with correctly
/// sized `TX_QUEUE` (a single step emits only a handful of frames); excess
/// frames are dropped rather than blocking the core.
fn queue_sink(q: &mut heapless::Vec<CanFrame, TX_QUEUE>) -> impl FnMut(CanFrame) + '_ {
    move |frame| {
        let overflow = q.push(frame).is_err();
        debug_assert!(!overflow, "tx queue overflow");
    }
}

async fn flush(q: &mut heapless::Vec<CanFrame, TX_QUEUE>, bus: &mut impl NodeBus) {
    for frame in q.iter() {
        bus.send(*frame).await;
    }
    q.clear();
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use canopen_core::od::{self, DataType, EntryInfo, OdError, PdoAccess, SdoAccess};
    use canopen_core::{Node, NodeId};
    use std::sync::mpsc;
    use std::vec::Vec;

    /// Bus backed by std channels; `recv` yields to the executor while empty.
    struct ChannelBus {
        rx: mpsc::Receiver<CanFrame>,
        tx: mpsc::Sender<CanFrame>,
    }

    impl NodeBus for ChannelBus {
        async fn recv(&mut self) -> CanFrame {
            loop {
                if let Ok(frame) = self.rx.try_recv() {
                    return frame;
                }
                // Poll-yield via a short timer so select() stays responsive.
                Timer::after_millis(1).await;
            }
        }

        async fn send(&mut self, frame: CanFrame) {
            self.tx.send(frame).unwrap();
        }
    }

    struct MiniOd {
        heartbeat_ms: u16,
    }

    impl ObjectDictionary for MiniOd {
        fn info(&self, index: u16, sub: u8) -> Result<EntryInfo, OdError> {
            match (index, sub) {
                (0x1017, 0) => Ok(EntryInfo {
                    data_type: DataType::Unsigned16,
                    sdo: SdoAccess::ReadWrite,
                    pdo: PdoAccess::No,
                    size: 2,
                }),
                _ => Err(OdError::ObjectNotFound),
            }
        }
        fn read(&self, index: u16, sub: u8, buf: &mut [u8]) -> Result<usize, OdError> {
            match (index, sub) {
                (0x1017, 0) => od::read_bytes(buf, &self.heartbeat_ms.to_le_bytes()),
                _ => Err(OdError::ObjectNotFound),
            }
        }
        fn write(&mut self, index: u16, sub: u8, data: &[u8]) -> Result<(), OdError> {
            match (index, sub) {
                (0x1017, 0) => {
                    self.heartbeat_ms = u16::from_le_bytes(od::exact::<2>(data)?);
                    Ok(())
                }
                _ => Err(OdError::ObjectNotFound),
            }
        }
    }

    /// Minimal block_on for the std embassy-time driver.
    fn block_on<F: core::future::Future>(fut: F) -> F::Output {
        use core::task::{Context, Poll, Waker};
        let mut fut = core::pin::pin!(fut);
        let waker = Waker::noop();
        let mut cx = Context::from_waker(&waker);
        loop {
            if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
                return out;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    }

    #[test]
    fn boot_heartbeat_sdo_and_reset() {
        let (to_node_tx, to_node_rx) = mpsc::channel();
        let (from_node_tx, from_node_rx) = mpsc::channel();
        let mut bus = ChannelBus { rx: to_node_rx, tx: from_node_tx };
        let mut node = Node::new(NodeId::new(7).unwrap(), MiniOd { heartbeat_ms: 20 });

        // Feed an SDO read of 0x1017 and then a communication reset.
        to_node_tx
            .send(CanFrame::new(0x607, &[0x40, 0x17, 0x10, 0x00, 0, 0, 0, 0]).unwrap())
            .unwrap();
        let reset = block_on(async {
            // Give the node ~65 ms: boot-up + >= 2 heartbeats + SDO response.
            let run = run(&mut node, &mut bus);
            match select(run, Timer::after_millis(65)).await {
                Either::First(reset) => Some(reset),
                Either::Second(()) => None,
            }
        });
        assert_eq!(reset, None, "no reset requested yet");

        let frames: Vec<CanFrame> = from_node_rx.try_iter().collect();
        assert!(!frames.is_empty(), "node must have transmitted");
        assert_eq!(frames[0], CanFrame::new(0x707, &[0x00]).unwrap(), "boot-up first");
        let heartbeats = frames.iter().filter(|f| f.id() == 0x707 && f.data() == [0x7F]).count();
        assert!(heartbeats >= 2, "expected cyclic heartbeats, got {heartbeats}");
        assert!(
            frames.iter().any(|f| f.id() == 0x587 && f.data()[0] == 0x4B),
            "expected SDO response"
        );

        // Now request a communication reset and expect run() to return.
        to_node_tx.send(CanFrame::new(0x000, &[0x82, 7]).unwrap()).unwrap();
        let reset = block_on(run(&mut node, &mut bus));
        assert_eq!(reset, ResetCommand::Communication);
    }
}
