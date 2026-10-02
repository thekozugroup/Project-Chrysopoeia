//! Print what Chrysopoeia detects on this machine as JSON.
//!
//! ```text
//! cargo run -p chrysopoeia-hwdetect --example detect [-- --root <dir>] [--no-verify]
//! ```
//!
//! Useful when setting up GPU passthrough: run it inside the container and
//! read the `hints`.

use std::path::PathBuf;

use chrysopoeia_hwdetect::{DetectOptions, detect};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut opts = DetectOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                if let Some(root) = args.next() {
                    opts.system_root = PathBuf::from(root);
                }
            }
            "--no-verify" => opts.verify_encoders = false,
            other => eprintln!("Ignoring unknown argument {other:?}."),
        }
    }
    let info = detect(&opts).await;
    match serde_json::to_string_pretty(&info) {
        Ok(json) => println!("{json}"),
        Err(err) => eprintln!("Could not format the result: {err}."),
    }
}
