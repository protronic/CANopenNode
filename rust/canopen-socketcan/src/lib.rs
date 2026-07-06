//! Linux SocketCAN transport for `canopen-core`.
//!
//! The Rust counterpart of the upstream
//! [CANopenLinux](https://github.com/CANopenNode/CANopenLinux) `CO_driver.c`:
//! it moves [`canopen_core::CanFrame`]s over a (v)can interface. Used for
//! host-side testing against reference implementations and for CANopen
//! nodes running on embedded Linux.

use std::io;
use std::time::Duration;

use canopen_core::CanFrame;
use socketcan::{CanSocket, Socket};

/// A blocking SocketCAN bus carrying CANopen frames.
pub struct SocketCanBus {
    socket: CanSocket,
}

impl SocketCanBus {
    /// Open a CAN interface, e.g. `"can0"` or `"vcan0"`.
    pub fn open(interface: &str) -> io::Result<Self> {
        let socket = CanSocket::open(interface).map_err(io::Error::other)?;
        Ok(Self { socket })
    }

    /// Send one frame.
    pub fn send(&self, frame: &CanFrame) -> io::Result<()> {
        let raw: socketcan::CanFrame = frame.to_embedded();
        self.socket.write_frame(&raw)
    }

    /// Receive the next CANopen-relevant frame, waiting up to `timeout`.
    ///
    /// Returns `Ok(None)` on timeout. Frames CANopen never uses (extended
    /// ids, remote frames, error frames) are skipped silently.
    pub fn recv(&self, timeout: Duration) -> io::Result<Option<CanFrame>> {
        // A zero SO_RCVTIMEO means "block forever" on POSIX sockets — a
        // deadline that has just elapsed must poll, not hang.
        self.socket.set_read_timeout(timeout.max(Duration::from_micros(1)))?;
        loop {
            match self.socket.read_frame() {
                Ok(raw) => {
                    if let socketcan::CanFrame::Data(data_frame) = raw {
                        if let Some(frame) = CanFrame::from_embedded(&data_frame) {
                            return Ok(Some(frame));
                        }
                    }
                    // Non-CANopen frame: keep waiting within the caller's
                    // next timeout window.
                }
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    return Ok(None);
                }
                // A signal (e.g. terminal resize) is not a bus failure.
                Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }
}
