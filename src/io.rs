//! Syncronous IO functions
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    ops::Range,
    result,
};

use blake3::guts::parent_cv;
use range_collections::{RangeSet2, RangeSetRef};
use smallvec::SmallVec;

use crate::{
    hash_block, hash_chunk,
    iter::{BaoChunk, ChunkIterRef, PostOrderChunkIter},
    outboard::Outboard,
    range_ok, BaoTree, BlockSize, ByteNum, ChunkNum, MapWithRef, TreeNode,
};

pub fn encode_ranges<D: Read + Seek, O: Outboard, W: Write>(
    data: D,
    outboard: O,
    ranges: &RangeSetRef<ChunkNum>,
    encoded: W,
) -> result::Result<(), DecodeError> {
    let mut data = data;
    let mut encoded = encoded;
    let file_len = ByteNum(data.seek(SeekFrom::End(0))?);
    let tree = outboard.tree();
    let ob_len = tree.size;
    if file_len != ob_len {
        return Err(DecodeError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("length from outboard does not match actual file length: {ob_len:?} != {file_len:?}"),
        )));
    }
    if !range_ok(ranges, tree.chunks()) {
        return Err(DecodeError::InvalidQueryRange);
    }
    let mut buffer = vec![0u8; tree.chunk_group_bytes().to_usize()];
    // write header
    encoded.write_all(tree.size.0.to_le_bytes().as_slice())?;
    for item in tree.read_item_iter_ref(ranges, 0) {
        match item {
            BaoChunk::Parent { node, .. } => {
                let (l_hash, r_hash) = outboard.load(node)?.unwrap();
                encoded.write_all(l_hash.as_bytes())?;
                encoded.write_all(r_hash.as_bytes())?;
            }
            BaoChunk::Leaf {
                start_chunk, size, ..
            } => {
                let start = start_chunk.to_bytes();
                let data = read_range_io(&mut data, start..start + (size as u64), &mut buffer)?;
                encoded.write_all(data)?;
            }
        }
    }
    Ok(())
}

pub fn encode_validated<D: Read + Seek, O: Outboard + Clone, W: Write>(
    mut data: D,
    outboard: O,
    mut encoded: W,
) -> io::Result<()> {
    let range = RangeSet2::from(ChunkNum(0)..);
    encode_ranges_validated(&mut data, outboard, &range, &mut encoded)?;
    Ok(())
}

pub fn encode_ranges_validated<D: Read + Seek, O: Outboard, W: Write>(
    data: D,
    outboard: O,
    ranges: &RangeSetRef<ChunkNum>,
    encoded: W,
) -> result::Result<(), DecodeError> {
    let mut stack = SmallVec::<[blake3::Hash; 10]>::new();
    stack.push(outboard.root());
    let mut data = data;
    let mut encoded = encoded;
    let file_len = ByteNum(data.seek(SeekFrom::End(0))?);
    let tree = outboard.tree();
    let ob_len = tree.size;
    if file_len != ob_len {
        return Err(DecodeError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("length from outboard does not match actual file length: {ob_len:?} != {file_len:?}"),
        )));
    }
    if !range_ok(ranges, tree.chunks()) {
        return Err(DecodeError::InvalidQueryRange);
    }
    let mut buffer = vec![0u8; tree.chunk_group_bytes().to_usize()];
    // write header
    encoded.write_all(tree.size.0.to_le_bytes().as_slice())?;
    for item in tree.read_item_iter_ref(ranges, 0) {
        match item {
            BaoChunk::Parent {
                is_root,
                left,
                right,
                node,
            } => {
                let (l_hash, r_hash) = outboard.load(node)?.unwrap();
                let actual = parent_cv(&l_hash, &r_hash, is_root);
                let expected = stack.pop().unwrap();
                if actual != expected {
                    return Err(DecodeError::ParentHashMismatch(node));
                }
                if right {
                    stack.push(r_hash);
                }
                if left {
                    stack.push(l_hash);
                }
                encoded.write_all(l_hash.as_bytes())?;
                encoded.write_all(r_hash.as_bytes())?;
            }
            BaoChunk::Leaf {
                start_chunk,
                size,
                is_root,
            } => {
                let expected = stack.pop().unwrap();
                let start = start_chunk.to_bytes();
                let data = read_range_io(&mut data, start..start + (size as u64), &mut buffer)?;
                let actual = hash_block(start_chunk, data, is_root);
                if actual != expected {
                    return Err(DecodeError::LeafHashMismatch(start_chunk));
                }
                encoded.write_all(data)?;
            }
        }
    }
    Ok(())
}
enum Position<'a> {
    /// currently reading the header, so don't know how big the tree is
    /// so we need to store the ranges and the chunk group log
    Header {
        ranges: &'a RangeSetRef<ChunkNum>,
        block_size: BlockSize,
    },
    /// currently reading the tree, all the info we need is in the iter
    Content { iter: ChunkIterRef<'a> },
}

pub struct DecodeSliceIter<'a, R> {
    inner: Position<'a>,
    stack: SmallVec<[blake3::Hash; 10]>,
    encoded: R,
    scratch: &'a mut [u8],
}

/// Error when decoding from a reader
///
/// This can either be a io error or a more specific error like a hash mismatch
#[derive(Debug)]
pub enum DecodeError {
    /// There was an error reading from the underlying io
    Io(io::Error),
    /// The hash of a parent did not match the expected hash
    ParentHashMismatch(TreeNode),
    /// The hash of a leaf did not match the expected hash
    LeafHashMismatch(ChunkNum),
    /// The query range was invalid
    InvalidQueryRange,
}

impl From<DecodeError> for io::Error {
    fn from(e: DecodeError) -> Self {
        match e {
            DecodeError::Io(e) => e,
            DecodeError::ParentHashMismatch(_) => {
                io::Error::new(io::ErrorKind::InvalidData, "parent hash mismatch")
            }
            DecodeError::LeafHashMismatch(_) => {
                io::Error::new(io::ErrorKind::InvalidData, "leaf hash mismatch")
            }
            DecodeError::InvalidQueryRange => {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid query range")
            }
        }
    }
}

impl From<io::Error> for DecodeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl<'a, R: Read> DecodeSliceIter<'a, R> {
    pub fn new(
        root: blake3::Hash,
        block_size: BlockSize,
        encoded: R,
        ranges: &'a RangeSetRef<ChunkNum>,
        scratch: &'a mut [u8],
    ) -> Self {
        // make sure the buffer is big enough
        assert!(scratch.len() >= block_size.size());
        let mut stack = SmallVec::new();
        stack.push(root);
        Self {
            stack,
            inner: Position::Header { ranges, block_size },
            encoded,
            scratch,
        }
    }

    pub fn buffer(&self) -> &[u8] {
        &self.scratch
    }

    pub fn tree(&self) -> Option<&BaoTree> {
        match &self.inner {
            Position::Content { iter } => Some(iter.tree()),
            Position::Header { .. } => None,
        }
    }

    fn next0(&mut self) -> result::Result<Option<Range<ByteNum>>, DecodeError> {
        loop {
            let inner = match &mut self.inner {
                Position::Content { ref mut iter } => iter,
                Position::Header {
                    block_size,
                    ranges: range,
                } => {
                    let size = read_len_io(&mut self.encoded)?;
                    // make sure the range is valid and canonical
                    if !range_ok(range, size.chunks()) {
                        break Err(DecodeError::InvalidQueryRange);
                    }
                    let tree = BaoTree::new(size, *block_size);
                    self.inner = Position::Content {
                        iter: tree.read_item_iter_ref(range, 0),
                    };
                    continue;
                }
            };
            match inner.next() {
                Some(BaoChunk::Parent {
                    is_root,
                    left,
                    right,
                    node,
                }) => {
                    let (l_hash, r_hash) = read_parent_io(&mut self.encoded)?;
                    let parent_hash = self.stack.pop().unwrap();
                    let actual = parent_cv(&l_hash, &r_hash, is_root);
                    if parent_hash != actual {
                        break Err(DecodeError::ParentHashMismatch(node));
                    }
                    if right {
                        self.stack.push(r_hash);
                    }
                    if left {
                        self.stack.push(l_hash);
                    }
                }
                Some(BaoChunk::Leaf {
                    size,
                    is_root,
                    start_chunk,
                }) => {
                    let buf = &mut self.scratch[..size];
                    self.encoded.read_exact(buf)?;
                    let actual = hash_block(start_chunk, buf, is_root);
                    let leaf_hash = self.stack.pop().unwrap();
                    if leaf_hash != actual {
                        break Err(DecodeError::LeafHashMismatch(start_chunk));
                    }
                    let start = start_chunk.to_bytes();
                    let end = start + (size as u64);
                    break Ok(Some(start..end));
                }
                None => break Ok(None),
            }
        }
    }
}

impl<'a, R: Read> Iterator for DecodeSliceIter<'a, R> {
    type Item = result::Result<Range<ByteNum>, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next0().transpose()
    }
}

/// Compute the post order outboard for the given data, writing into a io::Write
pub fn outboard_post_order_io(
    data: &mut impl Read,
    size: u64,
    block_size: BlockSize,
    outboard: &mut impl Write,
) -> io::Result<blake3::Hash> {
    let tree = BaoTree::new_with_start_chunk(ByteNum(size), block_size, ChunkNum(0));
    let mut buffer = vec![0; tree.chunk_group_bytes().to_usize()];
    let hash = outboard_post_order_sync_impl(tree, data, outboard, &mut buffer)?;
    outboard.write_all(&size.to_le_bytes())?;
    Ok(hash)
}

/// Compute the post order outboard for the given data
///
/// This is the internal version that takes a start chunk and does not append the size!
pub(crate) fn outboard_post_order_sync_impl(
    tree: BaoTree,
    data: &mut impl Read,
    outboard: &mut impl Write,
    buffer: &mut [u8],
) -> io::Result<blake3::Hash> {
    // do not allocate for small trees
    let mut stack = SmallVec::<[blake3::Hash; 10]>::new();
    debug_assert!(buffer.len() == tree.chunk_group_bytes().to_usize());
    for item in PostOrderChunkIter::new(tree) {
        match item {
            BaoChunk::Parent { is_root, .. } => {
                let right_hash = stack.pop().unwrap();
                let left_hash = stack.pop().unwrap();
                outboard.write_all(left_hash.as_bytes())?;
                outboard.write_all(right_hash.as_bytes())?;
                let parent = parent_cv(&left_hash, &right_hash, is_root);
                stack.push(parent);
            }
            BaoChunk::Leaf {
                size,
                is_root,
                start_chunk,
            } => {
                let buf = &mut buffer[..size];
                data.read_exact(buf)?;
                let hash = hash_block(start_chunk, buf, is_root);
                stack.push(hash);
            }
        }
    }
    debug_assert_eq!(stack.len(), 1);
    let hash = stack.pop().unwrap();
    Ok(hash)
}

/// Internal hash computation. This allows to also compute a non root hash, e.g. for a block
pub(crate) fn blake3_hash_inner(
    mut data: impl Read,
    data_len: ByteNum,
    start_chunk: ChunkNum,
    is_root: bool,
    buf: &mut [u8],
) -> std::io::Result<blake3::Hash> {
    let can_be_root = is_root;
    let mut stack = SmallVec::<[blake3::Hash; 10]>::new();
    let tree = BaoTree::new_with_start_chunk(data_len, BlockSize(0), start_chunk);
    for item in PostOrderChunkIter::new(tree) {
        match item {
            BaoChunk::Leaf {
                size,
                is_root,
                start_chunk,
            } => {
                let buf = &mut buf[..size];
                data.read_exact(buf)?;
                let hash = hash_chunk(start_chunk, buf, can_be_root && is_root);
                stack.push(hash);
            }
            BaoChunk::Parent { is_root, .. } => {
                let right_hash = stack.pop().unwrap();
                let left_hash = stack.pop().unwrap();
                let hash = parent_cv(&left_hash, &right_hash, can_be_root && is_root);
                stack.push(hash);
            }
        }
    }
    debug_assert_eq!(stack.len(), 1);
    Ok(stack.pop().unwrap())
}

pub fn decode_ranges_into<'a>(
    root: blake3::Hash,
    block_size: BlockSize,
    encoded: &'a mut impl Read,
    ranges: &'a RangeSetRef<ChunkNum>,
    scratch: &'a mut [u8],
    target: &'a mut (impl Write + Seek),
) -> impl Iterator<Item = std::io::Result<Range<ByteNum>>> + 'a {
    let iter = DecodeSliceIter::new(root, block_size, encoded, &ranges, scratch);
    let mut first = true;
    MapWithRef::new(iter, move |iter, item| match item {
        Ok(range) => {
            if first {
                let tree = iter.tree().unwrap();
                let target_len = ByteNum(target.seek(std::io::SeekFrom::End(0))?);
                if target_len < tree.size {
                    io_error!("target is too small")
                }
                first = false;
            }
            let len = (range.end - range.start).to_usize();
            let data = &iter.buffer()[..len];
            target.seek(std::io::SeekFrom::Start(range.start.0))?;
            target.write_all(data)?;
            Ok(range)
        }
        Err(e) => Err(e.into()),
    })
}

/// Decode encoded ranges given the root hash
#[cfg(test)]
pub fn decode_ranges_into_chunks<'a>(
    root: blake3::Hash,
    block_size: BlockSize,
    encoded: &'a mut impl Read,
    ranges: &'a RangeSetRef<ChunkNum>,
    scratch: &'a mut [u8],
) -> impl Iterator<Item = std::io::Result<(ByteNum, Vec<u8>)>> + 'a {
    let iter = DecodeSliceIter::new(root, block_size, encoded, &ranges, scratch);
    MapWithRef::new(iter, |iter, item| match item {
        Ok(range) => {
            let len = (range.end - range.start).to_usize();
            let data = &iter.buffer()[..len];
            Ok((range.start, data.to_vec()))
        }
        Err(e) => Err(e.into()),
    })
}

fn read_len_io(from: &mut impl Read) -> std::io::Result<ByteNum> {
    let mut buf = [0; 8];
    from.read_exact(&mut buf)?;
    let len = ByteNum(u64::from_le_bytes(buf));
    Ok(len)
}

fn read_parent_io(from: &mut impl Read) -> std::io::Result<(blake3::Hash, blake3::Hash)> {
    let mut buf = [0; 64];
    from.read_exact(&mut buf)?;
    let l_hash = blake3::Hash::from(<[u8; 32]>::try_from(&buf[..32]).unwrap());
    let r_hash = blake3::Hash::from(<[u8; 32]>::try_from(&buf[32..]).unwrap());
    Ok((l_hash, r_hash))
}

/// seeks read the bytes for the range from the source
fn read_range_io<'a>(
    from: &mut (impl Read + Seek),
    range: Range<ByteNum>,
    buf: &'a mut [u8],
) -> std::io::Result<&'a [u8]> {
    let len = (range.end - range.start).to_usize();
    from.seek(std::io::SeekFrom::Start(range.start.0))?;
    let mut buf = &mut buf[..len];
    from.read_exact(&mut buf)?;
    Ok(buf)
}