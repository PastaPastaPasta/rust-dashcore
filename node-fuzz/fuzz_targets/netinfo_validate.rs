//! `netinfo_validate` (dash-dml; a model target, EV1-1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("netinfo_validate");
