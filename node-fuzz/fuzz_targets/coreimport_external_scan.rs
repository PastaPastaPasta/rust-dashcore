//! `coreimport_external_scan` (dash-coreimport; a model target, DEC-59 Q1). Run only through
//! `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("coreimport_external_scan");
