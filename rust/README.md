# CANopenNode Rust Port

A native Rust port of the CANopenNode protocol stack, targeting
**STM32 with Embassy** (async, no_std) and **Linux/SocketCAN** (std, for
testing and embedded Linux devices).

## Why a port instead of FFI bindings?

Evaluated against the C code in this repository (v4.0):

- The C hot path is built from target-defined **macros** (`CO_LOCK_*`,
  `CO_CANrxMsg_read*`, `CO_FLAG_*`) that Rust cannot implement across FFI —
  the driver layer would have to stay C and would fight embassy-stm32's
  ownership of the CAN peripheral and its interrupts.
- Struct layouts (`CO_t` and friends) change shape with `CO_CONFIG_*`
  macros; bindgen output silently corrupts memory when configs drift.
- ~50+ `static inline` functions (the whole OD getter API) produce no
  linkable symbols for bindgen.

The C architecture itself, however, ports cleanly: no global state (one
`CO_t` tree), time injected via `timeDifference_us`/`timerNext_us`, and
per-object RX callbacks. This port keeps that architecture and uses the C
stack (CANopenLinux on `vcan`) as the interoperability reference.

## Architecture: sans-IO core

`canopen-core` contains pure protocol logic — no I/O, no clock, no alloc,
`no_std`:

- received frames go in via `on_frame(&frame, now, &mut tx)`,
- time-driven work runs in `process(now, &mut tx)` which returns the next
  deadline (the `timerNext_us` mechanism),
- outgoing frames leave through a caller-supplied sink closure,
- timestamps are `u64` microseconds supplied by the caller.

The same core is driven by an async Embassy runner on STM32 and by a
blocking SocketCAN loop on Linux; unit tests inject a fake clock.

| Crate | Role |
|---|---|
| `canopen-core` | Sans-IO protocol core (`no_std`, no alloc). |
| `canopen-socketcan` | Linux SocketCAN transport (std). |
| `canopen-demo` | CLI: heartbeat node, NMT master, SDO read/write. |

The Embassy adapter (bxCAN/FDCAN via `embassy-stm32`, `embassy-time`
runner) lives in the `protronic/embassy` fork and is the next milestone;
`canopen_core::CanFrame` already converts to/from any `embedded_can::Frame`
implementor, which covers both embassy-stm32 and socketcan frames.

## Status / roadmap

Ported (with unit tests):

- [x] CAN frame model, COB-ID connection set (`301/CO_ODinterface.h` ids)
- [x] NMT slave state machine + boot-up (`301/CO_NMT_Heartbeat.*`)
- [x] Heartbeat producer (OD 0x1017 semantics)
- [x] **SDO client, expedited transfers** (`301/CO_SDOclient.*`) — reads and
      writes parameters of other nodes from the device itself; this is a
      first-class feature of the port (zencan and friends only offer
      host-side clients)

Next, in order:

1. SDO client: segmented transfers (strings, >4 byte objects), then block
   transfer (`301/CO_SDOclient.*` remainder)
2. Object dictionary interface + codegen from EDS (`301/CO_ODinterface.*`,
   replacing CANopenEditor's generated `OD.c`/`OD.h`)
3. SDO server (`301/CO_SDOserver.*`)
4. Embassy runner + STM32 example in `protronic/embassy`
5. Emergency producer/consumer (`301/CO_Emergency.*`)
6. PDO + SYNC (`301/CO_PDO.*`, `301/CO_SYNC.*`)
7. Heartbeat consumer, NMT master, LSS (`305/`) as needed

## Testing

Unit tests (injected clock, no hardware):

```sh
cd rust && cargo test
```

Interop testing against the C reference on a virtual CAN bus:

```sh
sudo modprobe vcan
sudo ip link add dev vcan0 type vcan
sudo ip link set vcan0 up

# Reference node from https://github.com/CANopenNode/CANopenLinux
canopend vcan0 -i 4

# Rust side
cargo run -p canopen-demo -- sdo-read  vcan0 4 0x1017 0      # read heartbeat time
cargo run -p canopen-demo -- sdo-write vcan0 4 0x1017 0 500 2 # set to 500 ms
cargo run -p canopen-demo -- nmt vcan0 start 4                # NMT master command
cargo run -p canopen-demo -- node vcan0 10 1000               # run own node
```

`candump vcan0` alongside shows boot-up (`0x70A`), heartbeats and SDO
traffic.
