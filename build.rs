#[path = "build_support/resources.rs"]
mod resources;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support/resources.rs");
    // rust-i18n 4.2.4 reads catalogs in its macro without tracking those inputs.
    // Watching the directory also covers newly added and removed catalog files.
    println!("cargo:rerun-if-changed=locales");
    if let Err(error) = resources::build_resource_bundle() {
        panic!("Cannot bundle SlimeNRF resources: {error}");
    }
}
