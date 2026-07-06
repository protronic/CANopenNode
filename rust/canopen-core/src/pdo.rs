//! PDO (process data objects), port of `301/CO_PDO.*` with
//! `CO_CONFIG_PDO_BITWISE_MAPPING` semantics.
//!
//! Mapping and packing follow the C stack exactly so frames are
//! bit-compatible: the PDO payload is treated as one little-endian `u64`,
//! mapped application objects consume bits LSB-first in mapping order, the
//! frame length is `ceil(total bits / 8)` bytes, and received values are
//! zero-extended to the OD entry size before writing.
//!
//! Configuration is read from the OD (0x1400../0x1600.. for RPDOs,
//! 0x1800../0x1A00.. for TPDOs) when the [`crate::Node`] is created; like
//! the C stack without `CO_CONFIG_FLAG_OD_DYNAMIC`, changing PDO
//! configuration via SDO takes effect after the next communication reset.
//!
//! Supported transmission types: 254/255 (event-driven, with inhibit and
//! event timer). Synchronous types (0..=240) are accepted in the
//! configuration but do not fire until the SYNC object is ported.
//!
//! Like the C stack, the send request of an event-driven TPDO is held armed
//! while the node is not operational ("reset triggers" in
//! `CO_TPDO_process`), so every valid TPDO transmits once immediately upon
//! entering the operational state.

use crate::od::{ObjectDictionary, PdoAccess};
use crate::{CanFrame, Micros, TxSink};

/// Number of PDO slots scanned in the OD (0x1400+i / 0x1800+i).
pub const MAX_PDOS: usize = 4;
/// Maximum application objects mapped into one PDO (CiA 301 limit).
pub const MAX_MAPPED: usize = 8;

/// Event-driven transmission types (254 manufacturer / 255 profile).
const TRANSMISSION_EVENT: u8 = 254;

#[derive(Debug, Clone, Copy)]
struct MapEntry {
    index: u16,
    sub: u8,
    /// Mapped length in bits (bitwise mapping).
    bits: u8,
    /// Byte size of the OD entry, for zero-extended RPDO writes.
    size: u8,
}

#[derive(Debug, Clone, Copy)]
struct Mapping {
    entries: [Option<MapEntry>; MAX_MAPPED],
    total_bits: u16,
}

impl Mapping {
    /// Payload length in bytes: `ceil(total bits / 8)`.
    fn dlc(&self) -> usize {
        usize::from(self.total_bits).div_ceil(8)
    }
}

/// One configured transmit PDO.
#[derive(Debug)]
pub struct Tpdo {
    cob_id: u16,
    transmission: u8,
    inhibit_us: u64,
    event_us: u64,
    mapping: Mapping,
    send_request: bool,
    inhibit_until: Micros,
    event_deadline: Option<Micros>,
}

/// One configured receive PDO.
#[derive(Debug)]
pub struct Rpdo {
    cob_id: u16,
    mapping: Mapping,
}

/// Read the mapping parameter object (0x1600+i / 0x1A00+i) and resolve every
/// entry against the OD. Returns `None` (PDO disabled) on any erroneous
/// mapping, like the C stack flags `erroneousMap`.
fn read_mapping(od: &impl ObjectDictionary, map_index: u16, is_rpdo: bool) -> Option<Mapping> {
    let count = get::<1>(od, map_index, 0)?[0];
    if usize::from(count) > MAX_MAPPED {
        return None;
    }
    let mut mapping = Mapping {
        entries: [None; MAX_MAPPED],
        total_bits: 0,
    };
    for i in 0..count {
        let raw = u32::from_le_bytes(get::<4>(od, map_index, i + 1)?);
        let entry = resolve_entry(od, raw, is_rpdo)?;
        mapping.total_bits += u16::from(entry.bits);
        mapping.entries[usize::from(i)] = Some(entry);
    }
    if mapping.total_bits > 64 {
        return None;
    }
    Some(mapping)
}

/// Resolve one mapping entry `index << 16 | sub << 8 | length in bits`
/// against the OD: the entry must exist, be PDO-mappable in the right
/// direction and be long enough.
fn resolve_entry(od: &impl ObjectDictionary, raw: u32, is_rpdo: bool) -> Option<MapEntry> {
    let index = (raw >> 16) as u16;
    let sub = (raw >> 8) as u8;
    let bits = raw as u8;
    let info = od.info(index, sub).ok()?;
    let mappable = match info.pdo {
        PdoAccess::Both => true,
        PdoAccess::Rpdo => is_rpdo,
        PdoAccess::Tpdo => !is_rpdo,
        PdoAccess::No => false,
    };
    if !mappable || bits == 0 || usize::from(bits) > info.size * 8 || info.size > 8 {
        return None;
    }
    Some(MapEntry {
        index,
        sub,
        bits,
        size: info.size as u8,
    })
}

/// Read the COB-ID sub (0x..01): `None` if the PDO object is absent or the
/// valid bit (31) marks the PDO as disabled. Bit 30 (no RTR) is ignored —
/// this stack never sends RTR.
fn read_cob_id(od: &impl ObjectDictionary, comm_index: u16) -> Option<u16> {
    let cob = u32::from_le_bytes(get::<4>(od, comm_index, 1)?);
    if cob & 0x8000_0000 != 0 {
        return None;
    }
    Some((cob & 0x7FF) as u16)
}

impl Tpdo {
    /// Read TPDO configuration from the OD (comm 0x1800+slot, mapping
    /// 0x1A00+slot). `None` if absent, disabled or erroneously mapped.
    pub fn from_od(od: &impl ObjectDictionary, slot: usize) -> Option<Self> {
        let comm = 0x1800 + slot as u16;
        let cob_id = read_cob_id(od, comm)?;
        let transmission = get::<1>(od, comm, 2)?[0];
        // Inhibit time (0x..03, multiple of 100 µs) and event timer
        // (0x..05, ms) are optional subs.
        let inhibit_us = get::<2>(od, comm, 3)
            .map(|b| u64::from(u16::from_le_bytes(b)) * 100)
            .unwrap_or(0);
        let event_us = get::<2>(od, comm, 5)
            .map(|b| u64::from(u16::from_le_bytes(b)) * 1000)
            .unwrap_or(0);
        let mapping = read_mapping(od, 0x1A00 + slot as u16, false)?;
        Some(Self {
            cob_id,
            transmission,
            inhibit_us,
            event_us,
            mapping,
            // Armed like `CO_TPDO_init`: first process() in operational
            // transmits the initial PDO value.
            send_request: true,
            inhibit_until: 0,
            event_deadline: None,
        })
    }

    /// Whether this event-driven TPDO maps the given object (used to trigger
    /// transmission on SDO writes, the `OD_requestTPDO` mechanism).
    pub fn maps(&self, index: u16, sub: u8) -> bool {
        self.mapping
            .entries
            .iter()
            .flatten()
            .any(|e| e.index == index && e.sub == sub)
    }

    /// Request transmission at the next opportunity (`CO_TPDOsendRequest`).
    pub fn request(&mut self) {
        self.send_request = true;
    }

    /// Run the event/inhibit timers and transmit if due. `operational` gates
    /// transmission per NMT state. Returns the next deadline, if any.
    pub fn process(
        &mut self,
        od: &impl ObjectDictionary,
        operational: bool,
        now: Micros,
        tx: &mut impl TxSink,
    ) -> Option<Micros> {
        if self.transmission < TRANSMISSION_EVENT {
            // Synchronous TPDO: fires with the SYNC object (not ported yet).
            return None;
        }
        if !operational {
            // "Reset triggers" like the C stack: keep the send request
            // armed and clear the timers, so the TPDO transmits once
            // immediately on (re-)entering operational.
            self.send_request = true;
            self.event_deadline = None;
            self.inhibit_until = 0;
            return None;
        }
        // (Re)arm the event timer when entering operational.
        if self.event_deadline.is_none() && self.event_us > 0 {
            self.event_deadline = Some(now + self.event_us);
        }
        let event_due = self.event_deadline.is_some_and(|d| now >= d);
        if (self.send_request || event_due) && now >= self.inhibit_until {
            let (dlc, data) = pack(&self.mapping, od);
            tx.send(CanFrame::new(self.cob_id, &data[..dlc]).unwrap());
            self.send_request = false;
            self.inhibit_until = now + self.inhibit_us;
            if self.event_us > 0 {
                self.event_deadline = Some(now + self.event_us);
            }
        }
        match (
            self.event_deadline,
            (self.send_request).then_some(self.inhibit_until),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

impl Rpdo {
    /// Read RPDO configuration from the OD (comm 0x1400+slot, mapping
    /// 0x1600+slot). `None` if absent, disabled or erroneously mapped.
    ///
    /// Synchronous transmission types are treated as event-driven (values
    /// are applied on reception) until the SYNC object is ported.
    pub fn from_od(od: &impl ObjectDictionary, slot: usize) -> Option<Self> {
        let comm = 0x1400 + slot as u16;
        let cob_id = read_cob_id(od, comm)?;
        let mapping = read_mapping(od, 0x1600 + slot as u16, true)?;
        Some(Self { cob_id, mapping })
    }

    /// Apply a received frame if it matches this RPDO. Values are written to
    /// the OD zero-extended, like the C stack with bitwise mapping. Frames
    /// shorter than the mapped length are ignored.
    pub fn on_frame(&self, frame: &CanFrame, od: &mut impl ObjectDictionary) -> bool {
        if frame.id() != self.cob_id || frame.data().len() < self.mapping.dlc() {
            return false;
        }
        let mut word = [0u8; 8];
        word[..frame.data().len()].copy_from_slice(frame.data());
        let mut word = u64::from_le_bytes(word);

        for entry in self.mapping.entries.iter().flatten() {
            let value = word & mask(entry.bits);
            word >>= entry.bits;
            let bytes = value.to_le_bytes();
            // Mapping was validated against the OD; a failing write here
            // means the OD changed shape at runtime — ignore, as the C
            // stack only reports it as an emergency.
            let _ = od.write(entry.index, entry.sub, &bytes[..usize::from(entry.size)]);
        }
        true
    }
}

/// Pack mapped values LSB-first into the little-endian PDO word.
fn pack(mapping: &Mapping, od: &impl ObjectDictionary) -> (usize, [u8; 8]) {
    let mut word = 0u64;
    let mut shift = 0u32;
    for entry in mapping.entries.iter().flatten() {
        let mut buf = [0u8; 8];
        // Validated at config time; on read failure the field stays 0.
        let _ = od.read(entry.index, entry.sub, &mut buf);
        let value = u64::from_le_bytes(buf) & mask(entry.bits);
        word |= value << shift;
        shift += u32::from(entry.bits);
    }
    (mapping.dlc(), word.to_le_bytes())
}

fn mask(bits: u8) -> u64 {
    u64::MAX >> (64 - u32::from(bits))
}

/// Read an exactly-N-byte entry from the OD.
fn get<const N: usize>(od: &impl ObjectDictionary, index: u16, sub: u8) -> Option<[u8; N]> {
    let mut buf = [0u8; N];
    match od.read(index, sub, &mut buf) {
        Ok(n) if n == N => Some(buf),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // tests read clearer this way
mod tests {
    use super::*;
    use crate::od::{self, DataType, EntryInfo, OdError, SdoAccess};

    /// OD mirroring the example profile: 0x2000 (TPDO source: 2 bools, u8,
    /// u16), 0x2010 (RPDO sink), TPDO1/RPDO1 comm + mapping.
    struct PdoOd {
        t_bool1: u8,
        t_bool2: u8,
        t_u8: u8,
        r_bool1: u8,
        r_bool2: u8,
        r_u8: u8,
        tpdo_event_ms: u16,
        tpdo_inhibit_100us: u16,
        tpdo_cob: u32,
        map_count: u8,
    }

    impl Default for PdoOd {
        fn default() -> Self {
            Self {
                t_bool1: 0,
                t_bool2: 0,
                t_u8: 0,
                r_bool1: 0,
                r_bool2: 0,
                r_u8: 0,
                tpdo_event_ms: 0,
                tpdo_inhibit_100us: 0,
                tpdo_cob: 0x18A,
                map_count: 3,
            }
        }
    }

    impl ObjectDictionary for PdoOd {
        fn info(&self, index: u16, sub: u8) -> Result<EntryInfo, OdError> {
            let e = |data_type, pdo, size| {
                Ok(EntryInfo {
                    data_type,
                    sdo: SdoAccess::ReadWrite,
                    pdo,
                    size,
                })
            };
            match (index, sub) {
                (0x2000, 1 | 2) => e(DataType::Boolean, PdoAccess::Tpdo, 1),
                (0x2000, 5) => e(DataType::Unsigned8, PdoAccess::Tpdo, 1),
                (0x2010, 1 | 2) => e(DataType::Boolean, PdoAccess::Rpdo, 1),
                (0x2010, 5) => e(DataType::Unsigned8, PdoAccess::Rpdo, 1),
                _ => Err(OdError::ObjectNotFound),
            }
        }

        fn read(&self, index: u16, sub: u8, buf: &mut [u8]) -> Result<usize, OdError> {
            let b = |buf: &mut [u8], v: u8| od::read_bytes(buf, &[v]);
            match (index, sub) {
                (0x2000, 1) => b(buf, self.t_bool1),
                (0x2000, 2) => b(buf, self.t_bool2),
                (0x2000, 5) => b(buf, self.t_u8),
                (0x2010, 1) => b(buf, self.r_bool1),
                (0x2010, 2) => b(buf, self.r_bool2),
                (0x2010, 5) => b(buf, self.r_u8),
                // TPDO1 communication parameter.
                (0x1800, 1) => od::read_bytes(buf, &self.tpdo_cob.to_le_bytes()),
                (0x1800, 2) => b(buf, 254),
                (0x1800, 3) => od::read_bytes(buf, &self.tpdo_inhibit_100us.to_le_bytes()),
                (0x1800, 5) => od::read_bytes(buf, &self.tpdo_event_ms.to_le_bytes()),
                // TPDO1 mapping: 0x2000:01/1bit, :02/1bit, :05/8bit.
                (0x1A00, 0) => b(buf, self.map_count),
                (0x1A00, 1) => od::read_bytes(buf, &0x2000_0101u32.to_le_bytes()),
                (0x1A00, 2) => od::read_bytes(buf, &0x2000_0201u32.to_le_bytes()),
                (0x1A00, 3) => od::read_bytes(buf, &0x2000_0508u32.to_le_bytes()),
                // RPDO1 communication parameter.
                (0x1400, 1) => od::read_bytes(buf, &0x20Au32.to_le_bytes()),
                (0x1400, 2) => b(buf, 254),
                // RPDO1 mapping: 0x2010:01/1bit, :02/1bit, :05/8bit.
                (0x1600, 0) => b(buf, 3),
                (0x1600, 1) => od::read_bytes(buf, &0x2010_0101u32.to_le_bytes()),
                (0x1600, 2) => od::read_bytes(buf, &0x2010_0201u32.to_le_bytes()),
                (0x1600, 3) => od::read_bytes(buf, &0x2010_0508u32.to_le_bytes()),
                _ => Err(OdError::ObjectNotFound),
            }
        }

        fn write(&mut self, index: u16, sub: u8, data: &[u8]) -> Result<(), OdError> {
            let v = u8::from_le_bytes(od::exact::<1>(data)?);
            match (index, sub) {
                (0x2010, 1) => self.r_bool1 = v,
                (0x2010, 2) => self.r_bool2 = v,
                (0x2010, 5) => self.r_u8 = v,
                _ => return Err(OdError::ObjectNotFound),
            }
            Ok(())
        }
    }

    #[test]
    fn tpdo_packs_bits_lsb_first_like_c() {
        let mut od = PdoOd::default();
        od.t_bool1 = 1;
        od.t_bool2 = 0;
        od.t_u8 = 0xA5;
        let mut tpdo = Tpdo::from_od(&od, 0).unwrap();

        let mut sent = Vec::new();
        tpdo.request();
        tpdo.process(&od, true, 0, &mut |f| sent.push(f));

        // 10 bits -> DLC 2. Layout: bit0 = bool1, bit1 = bool2,
        // bits 2..10 = u8 value LSB-first.
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].id(), 0x18A);
        let expected = 0b1u64 | (0xA5u64 << 2);
        assert_eq!(sent[0].data(), &expected.to_le_bytes()[..2]);
    }

    #[test]
    fn rpdo_unpacks_and_writes_zero_extended() {
        let mut od = PdoOd::default();
        let rpdo = Rpdo::from_od(&od, 0).unwrap();

        let word = 0b0_1u64 | (1u64 << 1) | (0x42u64 << 2);
        let frame = CanFrame::new(0x20A, &word.to_le_bytes()[..2]).unwrap();
        assert!(rpdo.on_frame(&frame, &mut od));
        assert_eq!(od.r_bool1, 1);
        assert_eq!(od.r_bool2, 1);
        assert_eq!(od.r_u8, 0x42);

        // Foreign COB-ID and short frames are ignored.
        let foreign = CanFrame::new(0x20B, &[0, 0]).unwrap();
        assert!(!rpdo.on_frame(&foreign, &mut od));
        let short = CanFrame::new(0x20A, &[0]).unwrap();
        assert!(!rpdo.on_frame(&short, &mut od));
    }

    #[test]
    fn invalid_cob_id_bit_disables_pdo() {
        let mut od = PdoOd::default();
        od.tpdo_cob = 0x8000_018A; // bit 31: not valid
        assert!(Tpdo::from_od(&od, 0).is_none());
        od.tpdo_cob = 0x4000_018A; // bit 30 (no RTR) is fine
        assert!(Tpdo::from_od(&od, 0).is_some());
    }

    #[test]
    fn wrong_direction_mapping_is_rejected() {
        let od = PdoOd::default();
        // RPDO slot 0 maps 0x2010 (ok); a TPDO mapping 0x2010 must fail:
        // build via resolve on the raw entry.
        assert!(resolve_entry(&od, 0x2010_0101, true).is_some());
        assert!(resolve_entry(&od, 0x2010_0101, false).is_none());
        assert!(resolve_entry(&od, 0x2000_0101, false).is_some());
        // More bits than the entry has.
        assert!(resolve_entry(&od, 0x2000_0110, true).is_none());
        // Nonexistent object.
        assert!(resolve_entry(&od, 0x3000_0101, true).is_none());
    }

    #[test]
    fn event_timer_sends_cyclically() {
        let mut od = PdoOd::default();
        od.tpdo_event_ms = 50;
        od.t_u8 = 7;
        let mut tpdo = Tpdo::from_od(&od, 0).unwrap();
        let mut sent = Vec::new();

        // Not operational: nothing, no deadline.
        assert_eq!(tpdo.process(&od, false, 0, &mut |f| sent.push(f)), None);
        assert!(sent.is_empty());

        // Operational: initial transmission right away (C parity), then
        // the event timer fires every 50 ms.
        let next = tpdo.process(&od, true, 0, &mut |f| sent.push(f));
        assert_eq!(next, Some(50_000));
        assert_eq!(sent.len(), 1);
        tpdo.process(&od, true, 50_000, &mut |f| sent.push(f));
        assert_eq!(sent.len(), 2);
        tpdo.process(&od, true, 100_000, &mut |f| sent.push(f));
        assert_eq!(sent.len(), 3);
    }

    #[test]
    fn entering_operational_sends_once_even_without_event_timer() {
        let mut od = PdoOd::default();
        od.t_u8 = 0x11;
        let mut tpdo = Tpdo::from_od(&od, 0).unwrap();
        let mut sent = Vec::new();

        // Pre-operational process() must not send but keeps the request armed.
        assert_eq!(tpdo.process(&od, false, 0, &mut |f| sent.push(f)), None);
        assert!(sent.is_empty());

        // NMT start: the initial PDO value goes out once, then silence
        // until the next application request.
        tpdo.process(&od, true, 1_000, &mut |f| sent.push(f));
        assert_eq!(sent.len(), 1);
        tpdo.process(&od, true, 2_000_000, &mut |f| sent.push(f));
        assert_eq!(sent.len(), 1, "no event timer, no request -> no repeat");

        // Stop and restart: transmits once again on re-entering operational.
        tpdo.process(&od, false, 3_000_000, &mut |f| sent.push(f));
        tpdo.process(&od, true, 4_000_000, &mut |f| sent.push(f));
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn inhibit_time_delays_requests() {
        let mut od = PdoOd::default();
        od.tpdo_inhibit_100us = 10; // 1 ms
        let mut tpdo = Tpdo::from_od(&od, 0).unwrap();
        let mut sent = Vec::new();

        tpdo.request();
        tpdo.process(&od, true, 0, &mut |f| sent.push(f));
        assert_eq!(sent.len(), 1);

        // Second request within the inhibit window: deferred, deadline set.
        tpdo.request();
        let next = tpdo.process(&od, true, 500, &mut |f| sent.push(f));
        assert_eq!(sent.len(), 1);
        assert_eq!(next, Some(1_000));
        tpdo.process(&od, true, 1_000, &mut |f| sent.push(f));
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn erroneous_mapping_disables_pdo() {
        let mut od = PdoOd::default();
        od.map_count = 9; // more than 8 entries
        assert!(Tpdo::from_od(&od, 0).is_none());
        od.map_count = 4; // entry 4 does not exist in the OD
        assert!(Tpdo::from_od(&od, 0).is_none());
    }
}
