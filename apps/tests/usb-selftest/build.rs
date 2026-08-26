// SPDX-License-Identifier: Apache-2.0
fn main() {
    println!("cargo:rerun-if-env-changed=FLINT_USB_IMAGE_ID");
    let id = std::env::var("FLINT_USB_IMAGE_ID").unwrap_or_else(|_| "17200001".into());
    assert!(
        id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "USB image ID must be eight hex digits"
    );
    println!("cargo:rustc-env=FLINT_USB_IMAGE_ID={id}");
    build::link();
}
