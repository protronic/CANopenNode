//! Object dictionary code generation for the CANopenNode Rust port.
//!
//! Input: a device description exported by
//! [CANopenEditor](https://github.com/CANopenNode/CANopenEditor) in its
//! protobuf JSON format (`LibCanOpen.CanOpenDevice`, proto3 JSON mapping) —
//! the same editor project that exports EDS files for the rest of the
//! ecosystem.
//!
//! Output: Rust source with an `Od` struct (typed fields per OD entry, the
//! counterpart of the generated C `OD.c`/`OD.h`) implementing
//! `canopen_core::od::ObjectDictionary`.
//!
//! Intended use from a `build.rs`:
//!
//! ```no_run
//! let json = std::fs::read_to_string("device.codev.json").unwrap();
//! let code = canopen_od_codegen::generate(&json).unwrap();
//! let out = format!("{}/od_generated.rs", std::env::var("OUT_DIR").unwrap());
//! std::fs::write(out, code).unwrap();
//! ```

mod gen;
mod model;

pub use model::CanOpenDevice;

/// Generate Rust OD source code from a protobuf-JSON device description.
pub fn generate(json: &str) -> Result<String, String> {
    let device: CanOpenDevice =
        serde_json::from_str(json).map_err(|e| format!("invalid device JSON: {e}"))?;
    gen::generate(&device)
}
