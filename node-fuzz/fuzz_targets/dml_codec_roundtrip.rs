//! `dml_codec_roundtrip` (dash-dml; a model target, EV2a). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("dml_codec_roundtrip");
