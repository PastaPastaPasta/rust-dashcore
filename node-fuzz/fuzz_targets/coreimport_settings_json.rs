//! `coreimport_settings_json` (dash-coreimport; a model target, IM1a). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("coreimport_settings_json");
