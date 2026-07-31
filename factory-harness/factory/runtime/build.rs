use std::env;
use std::fs;
use std::path::PathBuf;

use sha2::Digest;
use sha2::Sha256;

const CODEX_V2_SCHEMA: &str =
    "../../codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.v2.schemas.json";

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo"),
    );
    let schema_path = manifest_dir.join(CODEX_V2_SCHEMA);
    println!("cargo:rerun-if-changed={}", schema_path.display());

    let schema = fs::read(&schema_path).unwrap_or_else(|error| {
        panic!(
            "failed to read Codex app-server V2 schema at {}: {error}",
            schema_path.display()
        )
    });
    let digest = Sha256::digest(schema);
    println!("cargo:rustc-env=FACTORY_CODEX_APP_SERVER_V2_SCHEMA_SHA256={digest:x}");
}
