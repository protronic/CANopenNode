//! Object dictionary generated at build time from
//! `example/DS301_profile.json` (the CiA 301 communication profile exported
//! by CANopenEditor as protobuf JSON).
//!
//! Serves as the reference user of `canopen-od-codegen` and as the OD for
//! examples and interop tests.

#![no_std]

mod generated {
    include!(concat!(env!("OUT_DIR"), "/od_generated.rs"));
}

pub use generated::*;
