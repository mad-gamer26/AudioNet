//! Generates the Swift (or Kotlin) bindings:
//! `cargo run -p audionet-ffi --features bindgen --bin uniffi-bindgen -- generate --library <lib> --language swift --out-dir <dir>`
fn main() {
    uniffi::uniffi_bindgen_main()
}
