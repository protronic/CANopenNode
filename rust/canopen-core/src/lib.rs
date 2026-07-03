//! Sans-IO CANopen (CiA 301) protocol core, ported from
//! [CANopenNode](https://github.com/CANopenNode/CANopenNode).
//!
//! # Design
//!
//! This crate contains pure protocol logic and owns no I/O and no clock,
//! mirroring the CANopenNode C architecture where every `CO_process*()`
//! function receives the elapsed time from the caller and returns a hint
//! for the next wake-up:
//!
//! * Received CAN frames are pushed in via `on_frame(&frame, now, &mut tx)`.
//! * Time-driven work happens in `process(now, &mut tx)`, which returns the
//!   next deadline so callers can sleep precisely (`timerNext_us` in C).
//! * Outgoing frames are emitted through a caller-provided sink closure.
//!
//! This makes the core trivially testable with an injected clock and lets the
//! same logic run on Embassy (async tasks, `embassy-time`) and on std Linux
//! (SocketCAN) without conditional compilation.
//!
//! Timestamps are `u64` microseconds from an arbitrary epoch (monotonic).

#![cfg_attr(not(test), no_std)]
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod cob;
pub mod frame;
pub mod heartbeat;
pub mod nmt;
pub mod node;
pub mod od;
pub mod sdo;

mod node_id;

pub use frame::CanFrame;
pub use node::{Node, NodeConfig, ResetCommand};
pub use node_id::NodeId;

/// Monotonic timestamp in microseconds from an arbitrary epoch.
pub type Micros = u64;

/// Sink for frames the protocol stack wants to transmit.
///
/// Implemented for any `FnMut(CanFrame)`; the caller decides whether that
/// pushes into an embassy channel, a SocketCAN socket or a test vector.
pub trait TxSink {
    /// Queue one frame for transmission.
    fn send(&mut self, frame: CanFrame);
}

impl<F: FnMut(CanFrame)> TxSink for F {
    fn send(&mut self, frame: CanFrame) {
        self(frame)
    }
}
