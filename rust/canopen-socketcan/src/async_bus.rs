//! Async [`NodeBus`] adapter for SocketCAN, so the `canopen-embassy` runner
//! drives a node on Linux exactly like on the MCU targets.
//!
//! A background thread performs the blocking socket reads and forwards
//! CANopen-relevant frames into an `embassy-sync` channel (the std stand-in
//! for the RX interrupt on embedded targets); `recv` awaits that channel.
//! Writes go directly to the shared socket — on CAN they complete
//! immediately unless the TX queue is exhausted.

use std::io;
use std::sync::Arc;

use canopen_core::CanFrame;
use canopen_embassy::NodeBus;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver};
use socketcan::{CanSocket, Socket};

const RX_QUEUE: usize = 64;
type RxChannel = Channel<CriticalSectionRawMutex, CanFrame, RX_QUEUE>;

/// Async SocketCAN bus for the `canopen-embassy` runner.
///
/// The reader thread lives for the rest of the process; create one bus per
/// process and interface.
pub struct AsyncSocketCanBus {
    socket: Arc<CanSocket>,
    rx: Receiver<'static, CriticalSectionRawMutex, CanFrame, RX_QUEUE>,
}

impl AsyncSocketCanBus {
    /// Open a CAN interface, e.g. `"can0"` or `"vcan0"`, and start the
    /// reader thread.
    pub fn open(interface: &str) -> io::Result<Self> {
        let socket = Arc::new(CanSocket::open(interface).map_err(io::Error::other)?);
        let channel: &'static RxChannel = Box::leak(Box::new(Channel::new()));

        let reader = Arc::clone(&socket);
        std::thread::spawn(move || loop {
            match reader.read_frame() {
                Ok(socketcan::CanFrame::Data(data_frame)) => {
                    if let Some(frame) = CanFrame::from_embedded(&data_frame) {
                        // If the executor falls behind, drop the frame — CAN
                        // has no flow control either.
                        let _ = channel.try_send(frame);
                    }
                }
                // Extended-id / remote / error frames: not CANopen.
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    eprintln!("CAN rx error: {e}");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        });

        Ok(Self {
            socket,
            rx: channel.receiver(),
        })
    }
}

impl NodeBus for AsyncSocketCanBus {
    async fn recv(&mut self) -> CanFrame {
        self.rx.receive().await
    }

    async fn send(&mut self, frame: CanFrame) {
        let raw: socketcan::CanFrame = frame.to_embedded();
        if let Err(e) = self.socket.write_frame(&raw) {
            eprintln!("CAN tx error: {e}");
        }
    }
}
