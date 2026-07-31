//! Rebuild trigger for the translation catalogs.
//!
//! `rust_i18n::i18n!` reads `locales/` while the macro expands, so the YAML is
//! an input to the build that cargo cannot see: editing a message without
//! touching a `.rs` file leaves cargo with nothing to do and the previous
//! strings baked into the binary. Declaring the directory here makes a catalog
//! edit invalidate the crate like any other source change.

fn main() {
    println!("cargo:rerun-if-changed=locales");
}
