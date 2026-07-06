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
| `canopen-od-codegen` | Generates the OD from a CANopenEditor protobuf-JSON export. |
| `canopen-example-od` | OD generated from `example/DS301_profile.json` (reference user). |
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
- [x] Object dictionary interface (`301/CO_ODinterface.*`) + **OD codegen**
      from CANopenEditor protobuf-JSON exports (`canopen-od-codegen`,
      replacing the generated `OD.c`/`OD.h`); verified against the full
      DS301 profile in `example/DS301_profile.json`
- [x] SDO server, expedited transfers (`301/CO_SDOserver.*`), integrated in
      `Node`: serves the generated OD with access/limit checks, respects NMT
      state, applies 0x1017 writes to the heartbeat producer immediately;
      end-to-end tested client ↔ server over the DS301 OD

Next, in order:

1. SDO client/server: segmented transfers (strings, >4 byte objects), then
   block transfer
2. Embassy runner + STM32 example in `protronic/embassy`
3. OD extensions (per-entry application callbacks, `OD_extension_init`) for
   DOMAIN objects and computed values
4. Emergency producer/consumer (`301/CO_Emergency.*`)
5. PDO + SYNC (`301/CO_PDO.*`, `301/CO_SYNC.*`)
6. Heartbeat consumer, NMT master, LSS (`305/`), storage (0x1010/0x1011)

## Object dictionary workflow

Author the OD in CANopenEditor as usual, then export **both** formats from
the same project: standard EDS for the ecosystem, protobuf JSON for this
port. The JSON is consumed at build time:

```rust
// build.rs
let json = std::fs::read_to_string("device.json").unwrap();
let code = canopen_od_codegen::generate(&json).unwrap();
std::fs::write(format!("{}/od_generated.rs", std::env::var("OUT_DIR").unwrap()), code).unwrap();
```

The generated `Od` struct has one typed field per entry (direct application
access, e.g. `od.x1017_producer_heartbeat_time`) and implements
`canopen_core::od::ObjectDictionary` — `(index, sub)` dispatch with access
rights, exact-length and limit checks, `$NODEID`-relative COB-IDs resolved
in `Od::new(node_id)`. `canopen-example-od` shows the pattern.

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

The demo node serves the DS301 example OD via SDO, so two shells and a vcan
are a complete self-test without the C reference:

```sh
cargo run -p canopen-demo -- node vcan0 10 &                  # device
cargo run -p canopen-demo -- sdo-read  vcan0 10 0x1200 2      # -> 0x58A ($NODEID+0x580)
cargo run -p canopen-demo -- sdo-write vcan0 10 0x1017 0 250 2 # heartbeat -> 250 ms
```

`candump vcan0` shows the heartbeat rate change immediately after the write.

`candump vcan0` alongside shows boot-up (`0x70A`), heartbeats and SDO
traffic.
