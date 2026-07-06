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
| `canopen-embassy` | Async runner: drives a `Node` from any `NodeBus` via `embassy-time`. |
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
- [x] SDO server (`301/CO_SDOserver.*`), integrated in `Node`: serves the
      generated OD with access/limit checks, respects NMT state, applies
      0x1017 writes to the heartbeat producer immediately; end-to-end tested
      client ↔ server over the DS301 OD
- [x] **Segmented SDO transfers** in client and server (strings and other
      objects >4 bytes) with toggle-bit checking, size verification and
      per-response timeouts (server aborts stale transfers); staged in a
      256-byte buffer (const-generic, no alloc); segmented client ↔
      segmented server roundtrips are cross-checked end-to-end

- [x] **Embassy runner** (`canopen-embassy`): async loop turning the node's
      `timerNext_us` hints into `embassy-time` timers; chip-independent via
      the `NodeBus` trait. STM32G4/FDCAN example:
      `examples/stm32g4/src/bin/canopen.rs` in `protronic/embassy`.
- [x] **PDO** (`301/CO_PDO.*`) with `CO_CONFIG_PDO_BITWISE_MAPPING`
      semantics (bit-granular mappings, frames bit-compatible with the C
      stack): configuration from 0x1400../0x1A00.. at node init with full
      mapping validation (direction, existence, length; erroneous mapping
      disables the PDO), event-driven TPDOs (types 254/255) with inhibit
      time, event timer and `tpdo_request()`; SDO writes to mapped objects
      trigger the TPDO (`OD_requestTPDO` mechanism); RPDOs write received
      values zero-extended, gated to the operational state. Verified over
      the profile's default mapping (0x2000 → TPDO1, RPDO1 → 0x2010).

Next, in order:

1. SYNC producer/consumer (`301/CO_SYNC.*`) + synchronous PDO transmission
   types (0..=240; accepted in config today but inactive)
2. Emergency producer/consumer (`301/CO_Emergency.*`)
3. OD extensions (per-entry application callbacks, `OD_extension_init`) for
   DOMAIN objects and computed values
4. Async `NodeBus` adapter for SocketCAN + master-side frame router (run the
   SDO client and the node on one bus/socket)
5. Heartbeat consumer, NMT master, LSS (`305/`), storage (0x1010/0x1011)
6. SDO block transfer — only needed for large DOMAIN transfers (firmware
   update), low priority

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

## Testing PDOs

The port implements the **bitwise mapping** variant
(`CO_CONFIG_PDO_BITWISE_MAPPING` in the C stack, and the only variant
here): mapping entries give the length in *bits*, several objects share
frame bytes LSB-first, and frames are bit-compatible with a C node compiled
with that option. The example profile maps by default:

| PDO | COB-ID (node 10) | Payload layout (LSB-first) |
|---|---|---|
| TPDO1 | 0x18A | bit 0 = 0x2000:01, bit 1 = 0x2000:02, bits 2–9 = 0x2000:05 |
| RPDO1 | 0x20A | bit 0 = 0x2010:01, bit 1 = 0x2010:02, bits 2–9 = 0x2010:05 |

PDOs are only active in NMT *operational* state:

```sh
cargo run -p canopen-demo -- node vcan0 10 &
candump vcan0 &
cargo run -p canopen-demo -- nmt vcan0 start 10
```

**TPDO** — an SDO write to a mapped object triggers the event-driven TPDO
(the `OD_requestTPDO` mechanism), so no application code is needed:

```sh
cargo run -p canopen-demo -- sdo-write vcan0 10 0x2000 5 0x42 1
# candump:  vcan0  18A  [2]  08 01     (0x42 << 2, 10 bits -> 2 bytes)
cargo run -p canopen-demo -- sdo-write vcan0 10 0x2000 1 1 1
# candump:  vcan0  18A  [2]  09 01     (bit 0 now set as well)
```

**RPDO** — inject a frame with can-utils, then read the values back via SDO:

```sh
cansend vcan0 20A#DD00     # bit0 = 1, bit1 = 0, u8 = 0xDD >> 2 = 0x37
cargo run -p canopen-demo -- sdo-read vcan0 10 0x2010 1   # -> 1
cargo run -p canopen-demo -- sdo-read vcan0 10 0x2010 5   # -> 55 (0x37)
```

**Cyclic TPDO** — set the event timer via SDO and apply it with a
communication reset. OD values survive the communication reset (like RAM
values in the C stack; an NMT *node* reset restores factory defaults):

```sh
cargo run -p canopen-demo -- sdo-write vcan0 10 0x1800 5 1000 2  # event timer 1 s
cargo run -p canopen-demo -- nmt vcan0 reset-comm 10             # apply PDO config
cargo run -p canopen-demo -- nmt vcan0 start 10
# candump: one 18A frame per second
```

The inhibit time (0x1800:03, 100 µs units) is applied the same way. On the
STM32 example the identical traffic appears on the physical bus; the
application there sets values via `node.od_mut()` and calls
`node.tpdo_request(0)`.


## eds File

### TPDO communication parameter

* COB-ID used by TPDO:
  * bit 31: If set, PDO does not exist / is not valid
  * bit 30: If set, NO RTR is allowed on this PDO
  * bit 11-29: set to 0
  * bit 0-10: 11-bit CAN-ID
* Transmission type:
  * Value 0: synchronous (acyclic)
  * Value 1-240: synchronous (cyclic every (1-240)-th sync)
  * Value 241-253: not used
  * Value 254: event-driven (manufacturer-specific)
  * Value 255: event-driven (device profile and application profile specific)
* Inhibit time in multiple of 100µs, if the transmission type is set to 254 or 255 (0 = disabled).
* Event timer interval in ms, if the transmission type is set to 254 or 255 (0 = disabled).
* SYNC start value
  * Value 0: Counter of the SYNC message shall not be processed.
  * Value 1-240: The SYNC message with the counter value equal to this value shall be regarded as the first received SYNC message.

### TPDO mapping parameter

* Number of mapped application objects in PDO:
  * Value 0: mapping is disabled.
  * Value 1: sub-index 0x01 is valid.
  * Value 2-8: sub-indexes 0x01 to (0x02 to 0x08) are valid.
* Application object 1-8:
  * bit 16-31: index
  * bit 8-15: sub-index
  * bit 0-7: data length in bits

### RPDO communication parameter

* COB-ID used by RPDO:
  * bit 31: If set, PDO does not exist / is not valid
  * bit 11-30: set to 0
  * bit 0-10: 11-bit CAN-ID
* Transmission type:
  * Value 0-240: synchronous, processed after next reception of SYNC object
  * Value 241-253: not used
  * Value 254: event-driven (manufacturer-specific)
  * Value 255: event-driven (device profile and application profile specific)
* Event timer in ms (0 = disabled) for deadline monitoring.

### RPDO mapping parameter

* Number of mapped application objects in PDO:
  * Value 0: mapping is disabled.
  * Value 1: sub-index 0x01 is valid.
  * Value 2-8: sub-indexes 0x01 to (0x02 to 0x08) are valid.
* Application object 1-8:
  * bit 16-31: index
  * bit 8-15: sub-index
  * bit 0-7: data length in bits
