//! CLI wrapper: `canopen-od-codegen <device.json> <output.rs>`

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: canopen-od-codegen <device.json> <output.rs>");
        return ExitCode::FAILURE;
    };
    let json = match std::fs::read_to_string(input) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("read {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match canopen_od_codegen::generate(&json) {
        Ok(code) => {
            if let Err(e) = std::fs::write(output, code) {
                eprintln!("write {output}: {e}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("codegen failed: {e}");
            ExitCode::FAILURE
        }
    }
}
