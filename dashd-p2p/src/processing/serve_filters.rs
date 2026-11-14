//! Basic compact-filter serving.

use std::collections::HashMap;
use std::sync::Arc;

use dash_consensus::primitives::write_compact_size;
use dash_num::Hash256;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, OutMsg, ServiceFlags};
use dash_p2p_core::{GetCFCheckpt, GetCFHeaders, GetCFilters};
use dash_primitives::BlockHash;
use dash_types::Numeric;

use super::registry::{HandlerCx, NetHandler};
use super::seams::EntryFacts;
use super::serve::block_request_allowed;

/// Maximum filters in one request.
pub const MAX_GETCFILTERS_SIZE: u32 = 1000;
/// Maximum filter hashes in one request.
pub const MAX_GETCFHEADERS_SIZE: u32 = 2000;
/// Spacing of filter-header checkpoints.
pub const CFCHECKPT_INTERVAL: u32 = 1000;
const CF_HEADERS_CACHE_MAX_SZ: usize = 2000;

static SERVED: [CommandString; 3] =
    [CommandString::GETCFILTERS, CommandString::GETCFHEADERS, CommandString::GETCFCHECKPT];

/// The three compact-filter request handlers and their checkpoint-header cache.
#[derive(Default)]
pub struct FilterServer {
    headers: HashMap<BlockHash, Hash256>,
}

impl NetHandler for FilterServer {
    fn commands(&self) -> &'static [CommandString] {
        &SERVED
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        let mut data = payload;
        match *cmd {
            CommandString::GETCFILTERS => {
                let req: GetCFilters = cx.read(&mut data)?;
                if let Some(stop) = prepare(
                    cx,
                    req.filter_type(),
                    req.start_height(),
                    req.stop_hash(),
                    MAX_GETCFILTERS_SIZE,
                ) {
                    self.cfilters(cx, req.start_height(), &stop);
                }
            }
            CommandString::GETCFHEADERS => {
                let req: GetCFHeaders = cx.read(&mut data)?;
                if let Some(stop) = prepare(
                    cx,
                    req.filter_type(),
                    req.start_height(),
                    req.stop_hash(),
                    MAX_GETCFHEADERS_SIZE,
                ) {
                    self.cfheaders(cx, req.start_height(), &stop);
                }
            }
            CommandString::GETCFCHECKPT => {
                let req: GetCFCheckpt = cx.read(&mut data)?;
                if let Some(stop) = prepare(cx, req.filter_type(), 0, req.stop_hash(), u32::MAX) {
                    self.cfcheckpt(cx, &stop);
                }
            }
            _ => unreachable!("only compact-filter requests are registered"),
        }
        Ok(())
    }
}

/// Core v24.0.0 `PrepareBlockFilterRequest`, net_processing.cpp:3435-3486.
fn prepare(
    cx: &HandlerCx<'_>,
    filter_type: u8,
    start: u32,
    stop_hash: BlockHash,
    max: u32,
) -> Option<EntryFacts> {
    let id = cx.id();
    if filter_type != 0 || !cx.state.our_services.has(ServiceFlags::NODE_COMPACT_FILTERS) {
        tracing::debug!(target: "net", "peer {id} requested unsupported block filter type: {filter_type}");
        cx.disconnect("unsupported block filter type");
        return None;
    }
    let mut snap = Arc::clone(&cx.snap);
    let stop = loop {
        let Some(stop) = cx.chain.entry_by_hash(&stop_hash) else {
            tracing::debug!(target: "net", "peer {id} requested invalid block hash: {stop_hash}");
            cx.disconnect("invalid block filter stop");
            return None;
        };
        match block_request_allowed(cx, &snap, &stop) {
            Ok(true) => break stop,
            Ok(false) => {
                tracing::debug!(target: "net", "peer {id} requested invalid block hash: {stop_hash}");
                cx.disconnect("invalid block filter stop");
                return None;
            }
            Err(_) => snap = cx.chain.snapshot(),
        }
    };
    if start > stop.height {
        tracing::debug!(target: "net", "peer {id} sent invalid getcfilters/getcfheaders with start height {start} and stop height {}", stop.height);
        cx.disconnect("invalid block filter range");
        return None;
    }
    if stop.height - start >= max {
        tracing::debug!(target: "net", "peer {id} requested too many cfilters/cfheaders: {} / {max}", stop.height - start + 1);
        cx.disconnect("block filter range too large");
        return None;
    }
    if !cx.filter_index.available() {
        tracing::debug!(target: "net", "Filter index for supported type basic not found");
        return None;
    }
    Some(stop)
}

impl FilterServer {
    /// Core v24.0.0 `ProcessGetCFilters`, net_processing.cpp:3488-3517.
    fn cfilters(&self, cx: &HandlerCx<'_>, start: u32, stop: &EntryFacts) {
        let Some(filters) = cx.filter_index.filters(start, stop.id) else {
            tracing::debug!(target: "net", "Failed to find block filter in index: filter_type=basic, start_height={start}, stop_hash={}", stop.hash);
            return;
        };
        for (hash, filter) in filters {
            let mut payload = prefix(&hash);
            write_compact_size(filter.len() as u64, &mut payload);
            payload.extend_from_slice(&filter);
            cx.push(OutMsg::new(CommandString::CFILTER, payload));
        }
    }

    /// Core v24.0.0 `LookupFilterHeader`, index/blockfilterindex.cpp:449-477.
    fn header(&mut self, cx: &HandlerCx<'_>, entry: &EntryFacts) -> Option<Hash256> {
        let checkpoint = entry.height.is_multiple_of(CFCHECKPT_INTERVAL);
        if checkpoint && let Some(header) = self.headers.get(&entry.hash) {
            return Some(*header);
        }
        let header = cx.filter_index.filter_header(entry.id)?;
        if checkpoint && self.headers.len() < CF_HEADERS_CACHE_MAX_SZ {
            self.headers.insert(entry.hash, header);
        }
        Some(header)
    }

    /// Core v24.0.0 `ProcessGetCFHeaders`, net_processing.cpp:3519-3561.
    fn cfheaders(&mut self, cx: &HandlerCx<'_>, start: u32, stop: &EntryFacts) {
        let prev = if start == 0 {
            Hash256::ZERO
        } else {
            let Some(entry) =
                cx.chain.ancestor(stop.id, start - 1).and_then(|id| cx.chain.entry(id))
            else {
                return;
            };
            let Some(header) = self.header(cx, &entry) else {
                tracing::debug!(target: "net", "Failed to find block filter header in index: filter_type=basic, block_hash={}", entry.hash);
                return;
            };
            header
        };
        let Some(hashes) = cx.filter_index.filter_hashes(start, stop.id) else {
            tracing::debug!(target: "net", "Failed to find block filter hashes in index: filter_type=basic, start_height={start}, stop_hash={}", stop.hash);
            return;
        };
        let mut payload = prefix(&stop.hash);
        payload.extend_from_slice(prev.as_bytes());
        append_hashes(&mut payload, &hashes);
        cx.push(OutMsg::new(CommandString::CFHEADERS, payload));
    }

    /// Core v24.0.0 `ProcessGetCFCheckPt`, net_processing.cpp:3563-3601.
    fn cfcheckpt(&mut self, cx: &HandlerCx<'_>, stop: &EntryFacts) {
        let mut headers = vec![Hash256::ZERO; (stop.height / CFCHECKPT_INTERVAL) as usize];
        let mut id = stop.id;
        for i in (0..headers.len()).rev() {
            let height = (i as u32 + 1) * CFCHECKPT_INTERVAL;
            let Some(entry) = cx.chain.ancestor(id, height).and_then(|id| cx.chain.entry(id))
            else {
                return;
            };
            let Some(header) = self.header(cx, &entry) else {
                tracing::debug!(target: "net", "Failed to find block filter header in index: filter_type=basic, block_hash={}", entry.hash);
                return;
            };
            headers[i] = header;
            id = entry.id;
        }
        let mut payload = prefix(&stop.hash);
        append_hashes(&mut payload, &headers);
        cx.push(OutMsg::new(CommandString::CFCHECKPT, payload));
    }
}

fn prefix(hash: &BlockHash) -> Vec<u8> {
    let mut payload = vec![0];
    payload.extend_from_slice(hash.as_bytes());
    payload
}

fn append_hashes(payload: &mut Vec<u8>, hashes: &[Hash256]) {
    write_compact_size(hashes.len() as u64, payload);
    for hash in hashes {
        payload.extend_from_slice(hash.as_bytes());
    }
}
