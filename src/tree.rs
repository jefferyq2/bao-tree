//! Define a number of newtypes and operations on these newtypes
//!
//! Most operations are concerned with node indexes in an in order traversal of a binary tree.
use std::{
    fmt,
    ops::{Add, Div, Mul, Sub},
};

use range_collections::range_set::RangeSetEntry;

index_newtype! {
    /// A number of blake3 chunks.
    ///
    /// This is a newtype for u64.
    /// The blake3 chunk size is 1024 bytes.
    pub struct ChunkNum(pub u64);
}

pub(crate) const BLAKE3_CHUNK_SIZE: usize = 1024;

/// A block size.
///
/// Block sizes are powers of 2, with the smallest being 1024 bytes.
/// They are encoded as the power of 2, minus 10, so 1 is 1024 bytes, 2 is 2048 bytes, etc.
///
/// Since only powers of 2 are valid, the log2 of the size in bytes / 1024 is given in the
/// constructor. So a block size of 0 is 1024 bytes, 1 is 2048 bytes, etc.
///
/// The actual size in bytes can be computed with [BlockSize::bytes].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockSize(pub(crate) u8);

impl fmt::Display for BlockSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self, f)
    }
}

impl BlockSize {
    /// Create a block size from the log2 of the size in bytes / 1024
    ///
    /// 0 is 1024 bytes, 1 is 2048 bytes, etc.
    pub const fn from_chunk_log(chunk_log: u8) -> Self {
        Self(chunk_log)
    }

    /// Get the log2 of the number of 1 KiB blocks in a chunk.
    pub const fn chunk_log(self) -> u8 {
        self.0
    }

    /// The default block size, 1024 bytes
    ///
    /// This means that blocks and blake3 chunks are the same size.
    pub const ZERO: BlockSize = BlockSize(0);

    /// Number of bytes in a block at this level
    pub const fn bytes(self) -> usize {
        BLAKE3_CHUNK_SIZE << self.0
    }

    /// Compute a block size from bytes
    pub const fn from_bytes(bytes: u64) -> Option<Self> {
        if bytes.count_ones() != 1 {
            // must be a power of 2
            return None;
        }
        if bytes < 1024 {
            // must be at least 1024 bytes
            return None;
        }
        Some(Self((bytes.trailing_zeros() - 10) as u8))
    }

    /// Convert to an u32 for comparison with levels
    pub(crate) const fn to_u32(self) -> u32 {
        self.0 as u32
    }
}
