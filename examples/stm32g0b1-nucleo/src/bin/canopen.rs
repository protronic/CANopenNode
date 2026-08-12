//! CANopen node on the NUCLEO-G0B1RE, using the CANopenNode Rust port.
//!
//! The Nucleo board exposes **FDCAN1** on the morpho connector — attach a
//! CAN transceiver breakout there (raw TXD/RXD type like SN65HVD230/MCP2562,
//! not an SPI shield). Alternative: **FDCAN2** on PB0/PB1 (Arduino D3/D4).
//!
//! | Signal | MCU pin | Morpho (CN7) |
//! |---|---|---|
//! | FDCAN1 RX | PA11 | pin 7 |
//! | FDCAN1 TX | PA12 | pin 2 |
//!
//! Classic CAN at 500 kbit/s, node id 10. The device boots as a CANopen slave
//! with the DS301 example object dictionary: boot-up message, cyclic heartbeat,
//! NMT slave, SDO server and event-driven PDOs. Test from a Linux host with a
//! (USB) SocketCAN adapter and the canopen-demo CLI from this repo:
//!
//! ```sh
//! cargo run -p canopen-demo -- sdo-read  can0 10 0x1200 2
//! cargo run -p canopen-demo -- sdo-write can0 10 0x1017 0 250 2
//! cargo run -p canopen-demo -- nmt can0 start 10
//! ```

#![no_std]
#![no_main]

use canopen_core::{CanFrame, Node, NodeId, ResetCommand};
use canopen_embassy::NodeBus;
use canopen_example_od::Od;
use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::peripherals::FDCAN1;
use embassy_stm32::{bind_interrupts, can};
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    FDCAN1_IT0 => can::IT0InterruptHandler<FDCAN1>;
    FDCAN1_IT1 => can::IT1InterruptHandler<FDCAN1>;
});

const NODE_ID: u8 = 10;
const BITRATE: u32 = 500_000;

/// CANopen bus adapter for the embassy-stm32 FDCAN driver (classic frames).
struct FdcanBus<'d> {
    can: can::Can<'d>,
}

impl NodeBus for FdcanBus<'_> {
    async fn recv(&mut self) -> CanFrame {
        loop {
            match self.can.read().await {
                Ok(envelope) => {
                    if let Some(frame) = CanFrame::from_embedded(&envelope.frame) {
                        return frame;
                    }
                }
                Err(err) => warn!("CAN rx error: {}", err),
            }
        }
    }

    async fn send(&mut self, frame: CanFrame) {
        self.can.write(&frame.to_embedded::<can::Frame>()).await;
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());
    info!("CANopen node {} starting (FDCAN1 on PA11/PA12)", NODE_ID);

    let mut configurator = can::CanConfigurator::new(p.FDCAN1, p.PA11, p.PA12, Irqs);
    configurator.set_bitrate(BITRATE);
    configurator.properties().set_extended_filter(
        can::filter::ExtendedFilterSlot::_0,
        can::filter::ExtendedFilter::reject_all(),
    );
    configurator.properties().set_standard_filter(
        can::filter::StandardFilterSlot::_0,
        can::filter::StandardFilter::accept_all_into_fifo0(),
    );
    let mut bus = FdcanBus {
        can: configurator.start(can::OperatingMode::NormalOperationMode),
    };

    let node_id = NodeId::new(NODE_ID).unwrap();
    let mut od = Od::new(node_id);
    loop {
        let mut node = Node::new(node_id, od.clone());
        info!("boot-up, entering pre-operational");
        match canopen_embassy::run(&mut node, &mut bus).await {
            ResetCommand::Communication => {
                info!("NMT reset communication");
                od = node.od().clone();
            }
            ResetCommand::Node => {
                info!("NMT reset node -> system reset");
                cortex_m::peripheral::SCB::sys_reset();
            }
        }
    }
}
