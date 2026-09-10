//! Generates the Swift and Kotlin bindings from this crate's exported API.
//!
//! ```text
//! cargo run -p ferry-runtime --bin uniffi-bindgen -- generate \
//!     --library target/debug/libferry_runtime.dylib --language swift --out-dir macos/Ferry/Generated
//! ```
fn main() {
    uniffi::uniffi_bindgen_main();
}
