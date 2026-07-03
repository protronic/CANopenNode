fn main() {
    let input = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../example/DS301_profile.json"
    );
    println!("cargo:rerun-if-changed={input}");
    let json = std::fs::read_to_string(input).expect("read DS301_profile.json");
    let code = canopen_od_codegen::generate(&json).expect("OD code generation");
    let out = format!("{}/od_generated.rs", std::env::var("OUT_DIR").unwrap());
    std::fs::write(out, code).expect("write generated OD");
}
