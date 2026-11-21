//! AcceptToMemoryPool's consensus binding (MP1a-5): the checks `MemPoolAccept` and the reorg
//! filter call, bound onto the consensus code that already exists. Each function runs under V's
//! turn or hold, with the pool lock held by the caller, as Core runs them under `cs_main` and
//! `m_pool.cs` (`validation.cpp:735-775` at v24.0.0).

pub mod consensus;
pub mod reorg_filter;
pub mod special;
pub mod view;

#[cfg(test)]
mod tests;
