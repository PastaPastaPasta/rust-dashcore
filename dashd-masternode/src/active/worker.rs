//! Core parity: active/masternode.cpp:175-228; validationinterface.cpp:193 at v24.0.0.

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, SendError, Sender};
use std::thread::{self, JoinHandle};

use dash_consensus::BlockId;
use dash_dml::MnList;

use super::state::ListError;

// Core: active/masternode.cpp:132, :184-185 at v24.0.0
pub trait ListForBlock: Send + Sync {
    fn list_for_block(&self, id: BlockId) -> Result<Arc<MnList>, ListError>;
}

// Core: active/masternode.cpp:184-194; validationinterface.cpp:193 at v24.0.0
pub struct CheckedListForBlock<L> {
    source: L,
    no_wait_marked: fn() -> bool,
}

impl<L> CheckedListForBlock<L> {
    pub fn new(source: L, no_wait_marked: fn() -> bool) -> Self {
        Self {
            source,
            no_wait_marked,
        }
    }
}

impl<L: ListForBlock> ListForBlock for CheckedListForBlock<L> {
    fn list_for_block(&self, id: BlockId) -> Result<Arc<MnList>, ListError> {
        assert!(!(self.no_wait_marked)());
        self.source.list_for_block(id)
    }
}

// Core: active/masternode.cpp:175; validationinterface.cpp:193 at v24.0.0
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipEvent {
    pub new: BlockId,
    pub fork: Option<BlockId>,
    pub ibd: bool,
}

// Core: active/masternode.cpp:175-228; validationinterface.cpp:193 at v24.0.0
#[derive(Clone)]
pub struct TipListener(Sender<TipEvent>);

impl TipListener {
    pub fn updated_block_tip(
        &self,
        new: BlockId,
        fork: Option<BlockId>,
        ibd: bool,
    ) -> Result<(), SendError<TipEvent>> {
        self.0.send(TipEvent {
            new,
            fork,
            ibd,
        })
    }
}

// Core: active/masternode.cpp:175-228; validationinterface.cpp:193 at v24.0.0
pub fn spawn_tip_worker(
    mut process: impl FnMut(TipEvent) + Send + 'static,
) -> io::Result<(TipListener, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel();
    let worker = thread::Builder::new().name("am1-tip".into()).spawn(move || {
        for event in rx {
            process(event);
        }
    })?;
    Ok((TipListener(tx), worker))
}

#[cfg(test)]
mod tests;
