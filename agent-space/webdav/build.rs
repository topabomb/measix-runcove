#[path = "../build-support/helper.rs"]
mod helper;
fn main() {
    let package = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = package.parent().unwrap();
    let name = package.file_name().unwrap().to_str().unwrap();
    println!("cargo:rerun-if-changed={}", package.join("src").display());
    for input in helper::inputs(workspace, name).expect("helper build inputs") {
        println!("cargo:rerun-if-changed={}", input.display());
    }
    println!(
        "cargo:rustc-env=AGENT_SPACE_HELPER_BUILD_ID={}",
        helper::fingerprint(workspace, name).expect("helper source fingerprint")
    );
}
