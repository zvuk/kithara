//! Compile-time defaults generated from `app.yaml` (+ `.env`) by
//! `build.rs`. See `app.yaml` for the source of truth and the build
//! script for the codegen contract.

include!(concat!(env!("OUT_DIR"), "/app_config_baked.rs"));
