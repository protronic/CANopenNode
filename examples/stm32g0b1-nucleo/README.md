# CANopen demo node — NUCLEO-G0B1RE

Embassy firmware for the [NUCLEO-G0B1RE](https://www.st.com/en/evaluation-tools/nucleo-g0b1re.html)
(STM32G0B1RET6). Runs the DS301 example object dictionary from this repository as
a CANopen slave: boot-up, heartbeat, NMT, SDO server and event-driven PDOs.

## Hardware

The Nucleo has no on-board CAN transceiver. Wire a breakout (e.g. SN65HVD230,
MCP2562) to **FDCAN1** on the morpho connector:

| Signal | MCU pin | Morpho CN7 |
|---|---|---|
| FDCAN1 RX | PA11 | pin 7 |
| FDCAN1 TX | PA12 | pin 2 |

Connect CAN-H and CAN-L to your USB SocketCAN adapter. With only two bus nodes,
enable 120 Ω termination on at least one side.

Alternative pins: FDCAN2 on PB0 (RX) / PB1 (TX), Arduino connector D3/D4 — change
the peripheral and pins in `src/bin/canopen.rs` if you prefer those.

## Build and flash

Needs [Rust](https://rustup.rs/), the `thumbv6m-none-eabi` target, and
[probe-rs](https://probe.rs/) (the on-board ST-LINK is used automatically):

```sh
rustup target add thumbv6m-none-eabi
cargo install probe-rs-tools --locked

cd examples/stm32g0b1-nucleo
cargo run --release --bin canopen
```

Defmt logs stream over RTT. Classic CAN bitrate is **500 kbit/s**, node id **10**.

## Test from Linux

Bring up the USB adapter:

```sh
sudo ip link set can0 up type can bitrate 500000
```

Then use the host-side demo from the `rust/` workspace (same commands as on vcan):

```sh
cd rust
cargo run -p canopen-demo -- sdo-read  can0 10 0x1200 2
cargo run -p canopen-demo -- sdo-write can0 10 0x1017 0 250 2
cargo run -p canopen-demo -- nmt can0 start 10
cargo run -p canopen-demo -- sdo-write can0 10 0x2000 5 0x42 1
```

`candump can0` should show boot-up (`70A`), heartbeats and PDO traffic after NMT start.
