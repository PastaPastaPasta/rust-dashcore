//! CJ1-W4: the node's services behind `dashd-coinjoin`'s host traits. Built and tested here;
//! W5 installs them.

pub mod adapters;
pub mod mempool;

#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod tests;
