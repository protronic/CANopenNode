//! SDO (service data object) protocol, port of `301/CO_SDOclient.*` and
//! `301/CO_SDOserver.*`.
//!
//! The SDO client is a first-class citizen of this port: it runs in the
//! no_std core so an embedded device (e.g. an STM32 acting as the machine
//! controller) can parameterize other CANopen nodes over SDO, not just be
//! parameterized itself.
//!
//! Currently implemented: expedited transfers (data up to 4 bytes), which
//! cover the vast majority of parameterization traffic (u8/u16/u32/i*/f32
//! objects). Segmented and block transfers are next on the roadmap.

mod abort;
mod client;
mod server;

pub use abort::SdoAbortCode;
pub use client::{SdoClient, SdoClientError, SdoEvent, SdoTransferError, DEFAULT_SDO_TIMEOUT_US};
pub use server::{SdoServer, SdoServerEvent};
