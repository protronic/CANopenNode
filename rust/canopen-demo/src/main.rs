//! Demo CLI for the CANopen Rust port.
//!
//! ```text
//! canopen-demo node      <iface> <node-id> [heartbeat-ms]
//! canopen-demo nmt       <iface> <start|stop|preop|reset-node|reset-comm> <node-id|0>
//! canopen-demo sdo-read  <iface> <server-id> <index> <sub>
//! canopen-demo sdo-write <iface> <server-id> <index> <sub> <value> <1|2|4|str>
//! ```
//!
//! Numbers accept decimal or `0x`-prefixed hex. Typical parameterization
//! round against a CANopenLinux reference node on vcan0:
//!
//! ```text
//! canopen-demo sdo-read  vcan0 4 0x1017 0        # read heartbeat time
//! canopen-demo sdo-write vcan0 4 0x1017 0 500 2  # set it to 500 ms
//! ```

use std::process::ExitCode;
use std::time::{Duration, Instant};

use canopen_core::nmt::NmtCommand;
use canopen_core::sdo::{SdoClient, SdoEvent, SdoTransferError};
use canopen_core::{CanFrame, Node, NodeId, ResetCommand};
use canopen_example_od::Od;
use canopen_socketcan::SocketCanBus;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args_str: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args_str.as_slice() {
        ["node", iface, id, rest @ ..] => run_node(iface, id, rest.first().copied()),
        ["nmt", iface, cmd, id] => send_nmt(iface, cmd, id),
        ["sdo-read", iface, server, index, sub] => sdo_read(iface, server, index, sub),
        ["sdo-write", iface, server, index, sub, value, size] => {
            sdo_write(iface, server, index, sub, value, size)
        }
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "usage:
  canopen-demo node      <iface> <node-id> [heartbeat-ms]
  canopen-demo nmt       <iface> <start|stop|preop|reset-node|reset-comm> <node-id|0>
  canopen-demo sdo-read  <iface> <server-id> <index> <sub>
  canopen-demo sdo-write <iface> <server-id> <index> <sub> <value> <1|2|4|str>";

fn parse_num(s: &str) -> Result<u64, String> {
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|_| format!("invalid number: {s}"))
}

fn parse_node_id(s: &str) -> Result<NodeId, String> {
    let raw = parse_num(s)?;
    u8::try_from(raw)
        .ok()
        .and_then(NodeId::new)
        .ok_or_else(|| format!("node id must be 1..=127, got {s}"))
}

/// Run a CANopen device with the DS301 example object dictionary: boot-up,
/// heartbeat, NMT slave, SDO server (parameterize it via `sdo-write`!).
fn run_node(iface: &str, id: &str, heartbeat_ms: Option<&str>) -> Result<(), String> {
    let node_id = parse_node_id(id)?;
    let heartbeat_period_ms = match heartbeat_ms {
        Some(s) => u16::try_from(parse_num(s)?).map_err(|_| "heartbeat-ms out of range")?,
        None => 1000,
    };
    let bus = SocketCanBus::open(iface).map_err(|e| format!("open {iface}: {e}"))?;
    let started = Instant::now();
    let now_us = || started.elapsed().as_micros() as u64;

    println!("node {node_id} on {iface}, heartbeat {heartbeat_period_ms} ms, DS301 example OD");
    let mut od = Od::new(node_id);
    od.x1017_producer_heartbeat_time = heartbeat_period_ms;
    loop {
        let mut node = Node::new(node_id, od.clone());
        let mut tx = tx_sink(&bus);
        node.start(now_us(), &mut tx);
        println!("boot-up sent, state {:?}", node.nmt_state());

        let reset = 'run: loop {
            let now = now_us();
            let next = node.process(now, &mut tx);
            let timeout = next
                .map(|deadline| Duration::from_micros(deadline.saturating_sub(now)))
                .unwrap_or(Duration::from_millis(100));

            let received = bus
                .recv(timeout)
                .map_err(|e| format!("recv: {e}"))?;
            if let Some(frame) = received {
                let state_before = node.nmt_state();
                if let Some(reset) = node.on_frame(&frame, now_us(), &mut tx) {
                    break 'run reset;
                }
                if node.nmt_state() != state_before {
                    println!("NMT state: {:?} -> {:?}", state_before, node.nmt_state());
                }
            }
        };
        match reset {
            ResetCommand::Communication => {
                // OD values survive a communication reset (like RAM values
                // in the C stack), so SDO-written configuration - e.g. PDO
                // event timers - takes effect in the new node.
                println!("NMT reset communication -> re-initializing");
                od = node.od().clone();
            }
            // A real device would reset the MCU here; the demo re-initializes
            // with factory defaults, like a power cycle without storage.
            ResetCommand::Node => {
                println!("NMT reset node -> re-initializing (demo)");
                od = Od::new(node_id);
                od.x1017_producer_heartbeat_time = heartbeat_period_ms;
            }
        }
    }
}

/// Send a single NMT master command.
fn send_nmt(iface: &str, cmd: &str, id: &str) -> Result<(), String> {
    let command = match cmd {
        "start" => NmtCommand::Start,
        "stop" => NmtCommand::Stop,
        "preop" => NmtCommand::EnterPreOperational,
        "reset-node" => NmtCommand::ResetNode,
        "reset-comm" => NmtCommand::ResetCommunication,
        _ => return Err(format!("unknown NMT command: {cmd}")),
    };
    let addressed = u8::try_from(parse_num(id)?).map_err(|_| "node id must be 0..=127")?;
    if addressed > 127 {
        return Err("node id must be 0..=127 (0 = all nodes)".into());
    }
    let bus = SocketCanBus::open(iface).map_err(|e| format!("open {iface}: {e}"))?;
    let frame = CanFrame::new(0x000, &[command as u8, addressed]).unwrap();
    bus.send(&frame).map_err(|e| format!("send: {e}"))?;
    println!("NMT {cmd} sent to node {addressed}");
    Ok(())
}

/// Run one SDO transfer to completion over the bus. Returns the uploaded
/// bytes for reads, `None` for successful writes.
fn run_sdo_transfer(
    bus: &SocketCanBus,
    client: &mut SdoClient,
    request: CanFrame,
    now_us: &dyn Fn() -> u64,
) -> Result<Option<Vec<u8>>, String> {
    bus.send(&request).map_err(|e| format!("send: {e}"))?;
    loop {
        let now = now_us();
        let deadline = client.next_deadline().unwrap_or(now);
        let timeout = Duration::from_micros(deadline.saturating_sub(now).max(1_000));

        let received = bus.recv(timeout).map_err(|e| format!("recv: {e}"))?;
        let mut tx = tx_sink(bus);
        if let Some(frame) = received {
            match client.on_frame(&frame, now_us(), &mut tx) {
                Some(SdoEvent::UploadOk { data, .. }) => return Ok(Some(data.to_vec())),
                Some(SdoEvent::DownloadOk { .. }) => return Ok(None),
                Some(event @ SdoEvent::Failed { .. }) => return Err(describe_failure(&event)),
                None => {}
            }
        }
        if let Some(event @ SdoEvent::Failed { .. }) = client.poll(now_us(), &mut tx) {
            return Err(describe_failure(&event));
        }
    }
}

fn sdo_read(iface: &str, server: &str, index: &str, sub: &str) -> Result<(), String> {
    let server = parse_node_id(server)?;
    let index = u16::try_from(parse_num(index)?).map_err(|_| "index out of range")?;
    let sub = u8::try_from(parse_num(sub)?).map_err(|_| "sub-index out of range")?;

    let bus = SocketCanBus::open(iface).map_err(|e| format!("open {iface}: {e}"))?;
    let started = Instant::now();
    let now_us = move || started.elapsed().as_micros() as u64;

    let mut client = SdoClient::new(server);
    let request = client.upload(index, sub, now_us()).expect("client is idle");
    let bytes = run_sdo_transfer(&bus, &mut client, request, &now_us)?
        .expect("upload returns data");
    if bytes.len() <= 4 {
        let mut le = [0u8; 4];
        le[..bytes.len()].copy_from_slice(&bytes);
        println!(
            "{index:#06X}:{sub:#04X} = {} ({} byte{}, raw {bytes:02X?})",
            u32::from_le_bytes(le),
            bytes.len(),
            if bytes.len() == 1 { "" } else { "s" },
        );
    } else if bytes.iter().all(|b| (0x20..0x7F).contains(b)) {
        println!(
            "{index:#06X}:{sub:#04X} = \"{}\" ({} bytes)",
            String::from_utf8_lossy(&bytes),
            bytes.len(),
        );
    } else {
        println!("{index:#06X}:{sub:#04X} = {bytes:02X?} ({} bytes)", bytes.len());
    }
    Ok(())
}

fn sdo_write(
    iface: &str,
    server: &str,
    index: &str,
    sub: &str,
    value: &str,
    size: &str,
) -> Result<(), String> {
    let server = parse_node_id(server)?;
    let index = u16::try_from(parse_num(index)?).map_err(|_| "index out of range")?;
    let sub = u8::try_from(parse_num(sub)?).map_err(|_| "sub-index out of range")?;
    let data: Vec<u8> = match size {
        "str" => value.as_bytes().to_vec(),
        "1" | "2" | "4" => {
            let size: usize = size.parse().unwrap();
            let value = parse_num(value)?;
            if size < 8 && value >= 1u64 << (size * 8) {
                return Err(format!("value {value} does not fit into {size} byte(s)"));
            }
            value.to_le_bytes()[..size].to_vec()
        }
        _ => return Err("size must be 1, 2, 4 (bytes) or 'str'".into()),
    };

    let bus = SocketCanBus::open(iface).map_err(|e| format!("open {iface}: {e}"))?;
    let started = Instant::now();
    let now_us = move || started.elapsed().as_micros() as u64;

    let mut client = SdoClient::new(server);
    let request = client
        .download(index, sub, &data, now_us())
        .map_err(|e| format!("cannot start download: {e:?}"))?;
    run_sdo_transfer(&bus, &mut client, request, &now_us)?;
    println!("{index:#06X}:{sub:#04X} <- {value} written ({} bytes)", data.len());
    Ok(())
}

fn describe_failure(event: &SdoEvent) -> String {
    match event {
        SdoEvent::Failed { index, sub, error } => match error {
            SdoTransferError::Abort(code) => {
                format!("SDO abort for {index:#06X}:{sub:#04X}: {code}")
            }
            SdoTransferError::Timeout => {
                format!("SDO timeout for {index:#06X}:{sub:#04X} (no response from server)")
            }
            SdoTransferError::BufferTooSmall => {
                format!("value of {index:#06X}:{sub:#04X} exceeds the client buffer")
            }
            SdoTransferError::Protocol => "SDO protocol error (malformed response)".to_string(),
        },
        other => format!("unexpected SDO event: {other:?}"),
    }
}

/// Adapt the bus to the core's `TxSink` (a closure sending frames).
fn tx_sink(bus: &SocketCanBus) -> impl FnMut(CanFrame) + '_ {
    move |frame| {
        if let Err(e) = bus.send(&frame) {
            eprintln!("tx error: {e}");
        }
    }
}
