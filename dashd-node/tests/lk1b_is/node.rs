//! The Rust side of the S1e differential: NodeSut, with its records tap, behind [`IsSide`]
//! (PLAN-LK1b §6 S1d/S1e).
//!
//! `BLOCK` is NodeSut's block op, `CL_RECORD` its `cl_record` and the enforcement V schedules
//! after it (S1e Q3), `ISLOCK` its `is_lock`, `SPORK` `update_spork`, `RESTART` `restart_report`
//! (scenario (e)'s `-maxtipage=1` as [`NodeSut::restart_with_max_tip_age`], S1e Q4). Each op ends
//! with V settled and the records the tap wrote; the first op's include the load's start-up records,
//! as the fixture's first step carries the recorder's.

use std::sync::Arc;
use std::time::Duration;

use dash_consensus::primitives::decode_block;
use dash_evo::spork::SporkId;
use dash_primitives::BlockHash;
use dash_statedigest::Hash256;
use dashd_node::logging::Logger;
use dashd_node::sut::{Harness, LoadStatus, NodeSut, Restarted, Teardown};
use dashd_node::ui::Ui;
use tempfile::TempDir;

use super::{Fixture, IsSide, Observed, Op, RestartKind, clock, node_args, node_datadir};

/// NodeSut on a fresh datadir set up as `fixture`'s recorder was.
pub struct NodeSide {
    pub sut: NodeSut,
    _dir: TempDir,
}

impl NodeSide {
    /// The side for `fixture`, with `harness`'s settings and the records tap on.
    pub fn new(fixture: &Fixture, harness: Harness) -> Result<NodeSide, String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        node_datadir(dir.path(), &fixture.run);
        let args = node_args(&fixture.run);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let first = fixture.steps.first().ok_or("an empty fixture")?;
        let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
        // A connector takes one digest sink: the tap, unless the product's log is asked for.
        let harness = Harness {
            records: harness.statedigestlog.is_none(),
            ..harness
        };
        let sut = NodeSut::with_harness(dir.path(), &argv, clock(None, first), ui, harness)
            .map_err(|e| format!("NodeSut: {e:?}"))?;
        if let Some(down) = sut.down() {
            return Err(format!("NodeSut is down: {down}"));
        }
        Ok(NodeSide {
            sut,
            _dir: dir,
        })
    }

    /// The active tip, as the fixture prints hashes.
    pub fn tip(&self) -> Result<Hash256, String> {
        let chain = self.sut.chain().ok_or("down")?;
        let tip = chain.view_handle().view().snap.tip.tip_hash;
        Ok(Hash256::from_wire_bytes(*tip.as_bytes()))
    }

    fn restart(&mut self, kind: &RestartKind, args: &[String]) -> Result<Restarted, String> {
        let how = match kind {
            RestartKind::Clean => Teardown::Clean,
            RestartKind::Kill => Teardown::Kill,
        };
        let restarted = match args {
            [] => self.sut.restart_report(how),
            [arg] if arg == "-maxtipage=1" => {
                self.sut.restart_with_max_tip_age(how, Duration::from_secs(1))
            }
            _ => return Err(format!("restart arguments {args:?}")),
        };
        if restarted.load.status != LoadStatus::Success {
            return Err(format!("the load after the restart: {:?}", restarted.load));
        }
        Ok(restarted)
    }
}

impl IsSide for NodeSide {
    fn set_time(&mut self, mocktime: i64) {
        self.sut.clock().set_mocktime(mocktime);
    }

    fn apply(&mut self, op: &Op) -> Result<Observed, String> {
        let mut observed = Observed::default();
        match op {
            Op::Block {
                bytes,
                ..
            } => {
                let block = decode_block(&mut &bytes[..])
                    .map_err(|e| format!("block: {e:?}"))?
                    .into_value();
                let chain = self.sut.chain().ok_or("down")?;
                chain
                    .process_block(Arc::new(block), true, None, true)
                    .recv()
                    .map_err(|_| "V stopped")?;
                self.sut.settle();
                observed.rows = Some(self.sut.is_rows());
                observed.tip = Some(self.tip()?);
            }
            Op::ClRecord {
                height,
                hash,
                sig,
            } => {
                let hash = BlockHash::from(hash.to_wire_bytes());
                self.sut.cl_record(*height, hash, *sig).ok_or("down")?;
                self.sut.settle();
            }
            Op::IsLock {
                bytes,
                mined_height,
                ..
            } => {
                let mined = mined_height
                    .map(|h| u32::try_from(h).map_err(|_| format!("mined height {h}")))
                    .transpose()?;
                match self.sut.is_lock(bytes, mined) {
                    // Core drops the message the same way.
                    Ok(_) => {}
                    Err(class) if class == "disabled" || class == "known" => {}
                    Err(e) => return Err(format!("ISLOCK refused: {e}")),
                }
                self.sut.settle();
            }
            Op::Spork {
                id,
                value,
            } => {
                let id = SporkId(i32::try_from(*id).map_err(|_| format!("spork id {id}"))?);
                self.sut.sporks().ok_or("down")?.update_spork(id, *value).ok_or("unsigned")?;
                self.sut.settle();
            }
            Op::Restart {
                kind,
                args,
            } => {
                let restarted = self.restart(kind, args)?;
                self.sut.settle();
                observed.rows = Some(restarted.is_rows);
            }
        }
        observed.records = self.sut.take_records()?;
        Ok(observed)
    }
}
