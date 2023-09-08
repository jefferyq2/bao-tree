//! Iterators over BaoTree nodes
//!
//! Range iterators take a reference to the ranges, and therefore require a lifetime parameter.
//! They can be used without lifetime parameters using self referencing structs.
use std::fmt;

use range_collections::{RangeSet2, RangeSetRef};
use self_cell::self_cell;
use smallvec::SmallVec;

use crate::{BaoTree, BlockSize, ByteNum, ChunkNum, TreeNode};

/// Extended node info.
///
/// Some of the information is redundant, but it is convenient to have it all in one place.
///
/// Usually this is used within an iterator, so we hope that the compiler will optimize away
/// the redundant information.
#[derive(Debug, PartialEq, Eq)]
pub struct NodeInfo<'a> {
    /// the node
    pub node: TreeNode,
    /// ranges of the node and it's two children
    pub ranges: &'a RangeSetRef<ChunkNum>,
    /// left child intersection with the query range
    pub l_ranges: &'a RangeSetRef<ChunkNum>,
    /// right child intersection with the query range
    pub r_ranges: &'a RangeSetRef<ChunkNum>,
    /// the node is fully included in the query range
    pub full: bool,
    /// the node is a leaf for the purpose of this query
    pub query_leaf: bool,
    /// the node is the root node (needs special handling when computing hash)
    pub is_root: bool,
    /// true if this node is the last leaf, and it is <= half full
    pub is_half_leaf: bool,
}

/// Iterator over all nodes in a BaoTree in pre-order that overlap with a given chunk range.
///
/// This is mostly used internally
#[derive(Debug)]
pub struct PreOrderPartialIterRef<'a> {
    /// the tree we want to traverse
    tree: BaoTree,
    /// number of valid nodes, needed in node.right_descendant
    tree_filled_size: TreeNode,
    /// the maximum level that is skipped from the traversal if it is fully
    /// included in the query range.
    max_skip_level: u8,
    /// is root
    is_root: bool,
    /// stack of nodes to visit
    stack: SmallVec<[(TreeNode, &'a RangeSetRef<ChunkNum>); 8]>,
}

impl<'a> PreOrderPartialIterRef<'a> {
    /// Create a new iterator over the tree.
    pub fn new(tree: BaoTree, ranges: &'a RangeSetRef<ChunkNum>, max_skip_level: u8) -> Self {
        let mut stack = SmallVec::new();
        stack.push((tree.root(), ranges));
        Self {
            tree,
            tree_filled_size: tree.filled_size(),
            max_skip_level,
            stack,
            is_root: tree.is_root,
        }
    }

    /// Get a reference to the tree.
    pub fn tree(&self) -> &BaoTree {
        &self.tree
    }
}

impl<'a> Iterator for PreOrderPartialIterRef<'a> {
    type Item = NodeInfo<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let tree = &self.tree;
        loop {
            let (node, ranges) = self.stack.pop()?;
            if ranges.is_empty() {
                continue;
            }
            // the middle chunk of the node
            let mid = node.mid().to_chunks(tree.block_size);
            // the start chunk of the node
            let start = node.block_range().start.to_chunks(tree.block_size);
            // check if the node is fully included
            let full = ranges.boundaries().len() == 1 && ranges.boundaries()[0] <= start;
            // split the ranges into left and right
            let (l_ranges, r_ranges) = ranges.split(mid);
            // we can't recurse if the node is a leaf
            // we don't want to recurse if the node is full and below the minimum level
            let query_leaf = node.is_leaf() || (full && node.level() <= self.max_skip_level as u32);
            // recursion is just pushing the children onto the stack
            if !query_leaf {
                let l = node.left_child().unwrap();
                let r = node.right_descendant(self.tree_filled_size).unwrap();
                // push right first so we pop left first
                self.stack.push((r, r_ranges));
                self.stack.push((l, l_ranges));
            }
            let is_root = self.is_root;
            self.is_root = false;
            let is_half_leaf = !tree.is_persisted(node);
            // emit the node in any case
            break Some(NodeInfo {
                node,
                ranges,
                l_ranges,
                r_ranges,
                full,
                query_leaf,
                is_root,
                is_half_leaf,
            });
        }
    }
}

/// Iterator over all nodes in a BaoTree in post-order.
#[derive(Debug)]
pub struct PostOrderNodeIter {
    /// the overall number of nodes in the tree
    len: TreeNode,
    /// the current node, None if we are done
    curr: TreeNode,
    /// where we came from, used to determine the next node
    prev: Prev,
}

impl PostOrderNodeIter {
    /// Create a new iterator over the tree.
    pub fn new(tree: BaoTree) -> Self {
        Self {
            len: tree.filled_size(),
            curr: tree.root(),
            prev: Prev::Parent,
        }
    }

    fn go_up(&mut self, curr: TreeNode) {
        let prev = curr;
        (self.curr, self.prev) = if let Some(parent) = curr.restricted_parent(self.len) {
            (
                parent,
                if prev < parent {
                    Prev::Left
                } else {
                    Prev::Right
                },
            )
        } else {
            (curr, Prev::Done)
        };
    }
}

impl Iterator for PostOrderNodeIter {
    type Item = TreeNode;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let curr = self.curr;
            match self.prev {
                Prev::Parent => {
                    if let Some(child) = curr.left_child() {
                        // go left first when coming from above, don't emit curr
                        self.curr = child;
                        self.prev = Prev::Parent;
                    } else {
                        // we are a left or right leaf, go up and emit curr
                        self.go_up(curr);
                        break Some(curr);
                    }
                }
                Prev::Left => {
                    // no need to check is_leaf, since we come from a left child
                    // go right when coming from left, don't emit curr
                    self.curr = curr.right_descendant(self.len).unwrap();
                    self.prev = Prev::Parent;
                }
                Prev::Right => {
                    // go up in any case, do emit curr
                    self.go_up(curr);
                    break Some(curr);
                }
                Prev::Done => {
                    break None;
                }
            }
        }
    }
}

/// Iterator over all nodes in a BaoTree in pre-order.
#[derive(Debug)]
pub struct PreOrderNodeIter {
    /// the overall number of nodes in the tree
    len: TreeNode,
    /// the current node, None if we are done
    curr: TreeNode,
    /// where we came from, used to determine the next node
    prev: Prev,
}

impl PreOrderNodeIter {
    /// Create a new iterator over the tree.
    pub fn new(tree: BaoTree) -> Self {
        Self {
            len: tree.filled_size(),
            curr: tree.root(),
            prev: Prev::Parent,
        }
    }

    fn go_up(&mut self, curr: TreeNode) {
        let prev = curr;
        (self.curr, self.prev) = if let Some(parent) = curr.restricted_parent(self.len) {
            (
                parent,
                if prev < parent {
                    Prev::Left
                } else {
                    Prev::Right
                },
            )
        } else {
            (curr, Prev::Done)
        };
    }
}

impl Iterator for PreOrderNodeIter {
    type Item = TreeNode;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let curr = self.curr;
            match self.prev {
                Prev::Parent => {
                    if let Some(child) = curr.left_child() {
                        // go left first when coming from above
                        self.curr = child;
                        self.prev = Prev::Parent;
                    } else {
                        // we are a left or right leaf, go up
                        self.go_up(curr);
                    }
                    // emit curr before children (pre-order)
                    break Some(curr);
                }
                Prev::Left => {
                    // no need to check is_leaf, since we come from a left child
                    // go right when coming from left, don't emit curr
                    self.curr = curr.right_descendant(self.len).unwrap();
                    self.prev = Prev::Parent;
                }
                Prev::Right => {
                    // go up in any case
                    self.go_up(curr);
                }
                Prev::Done => {
                    break None;
                }
            }
        }
    }
}

#[derive(Debug)]
enum Prev {
    Parent,
    Left,
    Right,
    Done,
}

/// A chunk describes what to expect from a response stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseChunk {
    /// expect a 64 byte parent node.
    ///
    /// To validate, use parent_cv using the is_root value
    Parent {
        /// The tree node, useful for error reporting
        node: Option<TreeNode>,
        /// This is the root, to be passed to parent_cv
        is_root: bool,
        /// Push the left hash to the stack, since it will be needed later
        left: bool,
        /// Push the right hash to the stack, since it will be needed later
        right: bool,
    },
    /// expect data of size `size`
    ///
    /// To validate, use hash_block using the is_root and start_chunk values
    Leaf {
        /// Start chunk, to be passed to hash_block
        start_chunk: ChunkNum,
        /// Size of the data to expect. Will be chunk_group_bytes for all but the last block.
        size: usize,
        /// This is the root, to be passed to hash_block
        is_root: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A chunk describeds what to read or write next
///
/// In some cases you want additional information about what part of the chunk matches the query.
/// That is what the `R` type parameter is for. By default it is `()`.
pub enum BaoChunk<R = ()> {
    /// expect a 64 byte parent node.
    ///
    /// To validate, use parent_cv using the is_root value
    Parent {
        /// The tree node, useful for error reporting
        node: TreeNode,
        /// This is the root, to be passed to parent_cv
        is_root: bool,
        /// Push the left hash to the stack, since it will be needed later
        left: bool,
        /// Push the right hash to the stack, since it will be needed later
        right: bool,
        /// Additional information about what part of the chunk matches the query
        ranges: R,
    },
    /// expect data of size `size`
    ///
    /// To validate, use hash_block using the is_root and start_chunk values
    Leaf {
        /// Start chunk, to be passed to hash_block
        start_chunk: ChunkNum,
        /// Size of the data to expect. Will be chunk_group_bytes for all but the last block.
        size: usize,
        /// This is the root, to be passed to hash_block
        is_root: bool,
        /// Additional information about what part of the chunk matches the query
        ranges: R,
    },
}

impl<T> BaoChunk<T> {
    /// Create a dummy empty range
    fn empty(ranges: T) -> Self {
        Self::Leaf {
            size: 0,
            is_root: false,
            start_chunk: ChunkNum(0),
            ranges,
        }
    }

    /// Map the ranges of the chunk. Convenient way to get rid of a lifetime
    /// by just mapping to a type without a lifetime.
    fn map_ranges<U>(self, f: impl Fn(T) -> U) -> BaoChunk<U> {
        match self {
            Self::Parent {
                is_root,
                left,
                right,
                node,
                ranges,
            } => BaoChunk::Parent {
                is_root,
                left,
                right,
                node,
                ranges: f(ranges),
            },
            Self::Leaf {
                size,
                is_root,
                start_chunk,
                ranges,
            } => BaoChunk::Leaf {
                size,
                is_root,
                start_chunk,
                ranges: f(ranges),
            },
        }
    }
}

/// Iterator over all chunks in a BaoTree in post-order.
#[derive(Debug)]
pub struct PostOrderChunkIter {
    tree: BaoTree,
    inner: PostOrderNodeIter,
    // stack with 2 elements, since we can only have 2 items in flight
    stack: [BaoChunk; 2],
    index: usize,
    root: TreeNode,
}

impl PostOrderChunkIter {
    /// Create a new iterator over the tree.
    pub fn new(tree: BaoTree) -> Self {
        Self {
            tree,
            inner: PostOrderNodeIter::new(tree),
            stack: Default::default(),
            index: 0,
            root: tree.root(),
        }
    }

    fn push(&mut self, item: BaoChunk) {
        self.stack[self.index] = item;
        self.index += 1;
    }

    fn pop(&mut self) -> Option<BaoChunk> {
        if self.index > 0 {
            self.index -= 1;
            Some(self.stack[self.index])
        } else {
            None
        }
    }
}

impl Iterator for PostOrderChunkIter {
    type Item = BaoChunk;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.pop() {
                return Some(item);
            }
            let node = self.inner.next()?;
            let is_root = node == self.root;
            if self.tree.is_persisted(node) {
                self.push(BaoChunk::Parent {
                    node,
                    is_root,
                    left: true,
                    right: true,
                    ranges: (),
                });
            }
            if let Some(leaf) = node.as_leaf() {
                let tree = &self.tree;
                let (s, m, e) = tree.leaf_byte_ranges3(leaf);
                let l_start_chunk = tree.chunk_num(leaf);
                let r_start_chunk = l_start_chunk + tree.chunk_group_chunks();
                let is_half_leaf = m == e;
                if !is_half_leaf {
                    self.push(BaoChunk::Leaf {
                        is_root: false,
                        start_chunk: r_start_chunk,
                        size: (e - m).to_usize(),
                        ranges: (),
                    });
                };
                break Some(BaoChunk::Leaf {
                    is_root: is_root && is_half_leaf,
                    start_chunk: l_start_chunk,
                    size: (m - s).to_usize(),
                    ranges: (),
                });
            }
        }
    }
}

impl BaoChunk {
    /// Return the size of the chunk in bytes.
    pub fn size(&self) -> usize {
        match self {
            Self::Parent { .. } => 64,
            Self::Leaf { size, .. } => *size,
        }
    }
}

impl<T: Default> Default for BaoChunk<T> {
    fn default() -> Self {
        Self::Leaf {
            is_root: true,
            size: 0,
            start_chunk: ChunkNum(0),
            ranges: T::default(),
        }
    }
}

/// An iterator that produces chunks in pre order, but only for the parts of the
/// tree that are relevant for a query.
#[derive(Debug)]
pub struct PreOrderChunkIterRef<'a> {
    inner: PreOrderPartialIterRef<'a>,
    // stack with 2 elements, since we can only have 2 items in flight
    stack: [BaoChunk<&'a RangeSetRef<ChunkNum>>; 2],
    index: usize,
}

impl<'a> PreOrderChunkIterRef<'a> {
    /// Create a new iterator over the tree.
    pub fn new(tree: BaoTree, ranges: &'a RangeSetRef<ChunkNum>, max_skip_level: u8) -> Self {
        Self {
            inner: tree.ranges_pre_order_nodes_iter(ranges, max_skip_level),
            // todo: get rid of this when &RangeSetRef has a default
            stack: [BaoChunk::empty(ranges), BaoChunk::empty(ranges)],
            index: 0,
        }
    }

    /// Return a reference to the underlying tree.
    pub fn tree(&self) -> &BaoTree {
        self.inner.tree()
    }

    fn push(&mut self, item: BaoChunk<&'a RangeSetRef<ChunkNum>>) {
        self.stack[self.index] = item;
        self.index += 1;
    }

    fn pop(&mut self) -> Option<BaoChunk<&'a RangeSetRef<ChunkNum>>> {
        if self.index > 0 {
            self.index -= 1;
            Some(self.stack[self.index])
        } else {
            None
        }
    }
}

impl<'a> Iterator for PreOrderChunkIterRef<'a> {
    type Item = BaoChunk<&'a RangeSetRef<ChunkNum>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.pop() {
                return Some(item);
            }
            let NodeInfo {
                node,
                is_root,
                is_half_leaf,
                ranges,
                l_ranges,
                r_ranges,
                query_leaf,
                ..
            } = self.inner.next()?;
            if let Some(leaf) = node.as_leaf() {
                let tree = &self.inner.tree;
                let (s, m, e) = tree.leaf_byte_ranges3(leaf);
                let l_start_chunk = tree.chunk_num(leaf);
                let r_start_chunk = l_start_chunk + tree.chunk_group_chunks();
                if !r_ranges.is_empty() && !is_half_leaf {
                    self.push(BaoChunk::Leaf {
                        is_root: false,
                        start_chunk: r_start_chunk,
                        size: (e - m).to_usize(),
                        ranges: r_ranges,
                    });
                };
                if !l_ranges.is_empty() {
                    self.push(BaoChunk::Leaf {
                        is_root: is_root && is_half_leaf,
                        start_chunk: l_start_chunk,
                        size: (m - s).to_usize(),
                        ranges: l_ranges,
                    });
                };
            }
            // the last leaf is a special case, since it does not have a parent if it is <= half full
            if !is_half_leaf {
                let chunk = if query_leaf && !node.is_leaf() {
                    // the node is a leaf for the purpose of this query despite not being a leaf,
                    // so we need to return a BaoChunk::Leaf spanning the whole node
                    let tree = self.tree();
                    let bytes = tree.byte_range(node);
                    let start_chunk = bytes.start.chunks();
                    let size = (bytes.end.0 - bytes.start.0) as usize;
                    BaoChunk::Leaf {
                        start_chunk,
                        is_root,
                        size,
                        ranges,
                    }
                } else {
                    // the node is not a leaf, so we need to return a BaoChunk::Parent
                    BaoChunk::Parent {
                        is_root,
                        left: !l_ranges.is_empty(),
                        right: !r_ranges.is_empty(),
                        node,
                        ranges,
                    }
                };
                break Some(chunk);
            }
        }
    }
}

/// An iterator that produces chunks in pre order.
///
/// This wraps a `PreOrderPartialIterRef` and iterates over the chunk groups
/// all the way down to individual chunks if needed.
#[derive(Debug)]
pub struct ResponseIterRef<'a> {
    inner: PreOrderChunkIterRef<'a>,
    buffer: SmallVec<[ResponseChunk; 10]>,
}

impl<'a> ResponseIterRef<'a> {
    /// Create a new iterator over the tree.
    pub fn new(tree: BaoTree, ranges: &'a RangeSetRef<ChunkNum>, max_skip_level: u8) -> Self {
        Self {
            inner: PreOrderChunkIterRef::new(tree, ranges, max_skip_level),
            buffer: SmallVec::new(),
        }
    }

    /// Return a reference to the underlying tree.
    pub fn tree(&self) -> &BaoTree {
        self.inner.tree()
    }
}
impl<'a> Iterator for ResponseIterRef<'a> {
    type Item = ResponseChunk;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.buffer.pop() {
                break Some(item);
            }
            match self.inner.next()? {
                BaoChunk::Parent {
                    node,
                    is_root,
                    right,
                    left,
                    ..
                } => {
                    break Some(ResponseChunk::Parent {
                        node: Some(node),
                        is_root,
                        right,
                        left,
                    });
                }
                BaoChunk::Leaf {
                    size,
                    is_root,
                    start_chunk,
                    ranges,
                } => {
                    if self.tree().block_size == BlockSize(0) || ranges.is_all() {
                        break Some(ResponseChunk::Leaf {
                            size,
                            is_root,
                            start_chunk,
                        });
                    } else {
                        // create a little tree just for this leaf
                        let tree = BaoTree {
                            start_chunk,
                            size: ByteNum(size as u64),
                            block_size: BlockSize(0),
                            is_root,
                        };
                        for item in tree.ranges_pre_order_chunks_iter_ref(ranges, u8::MAX) {
                            match item {
                                BaoChunk::Parent {
                                    is_root,
                                    left,
                                    right,
                                    node,
                                    ..
                                } => {
                                    self.buffer.push(ResponseChunk::Parent {
                                        node: None,
                                        is_root,
                                        left,
                                        right,
                                    });
                                }
                                BaoChunk::Leaf {
                                    size,
                                    is_root,
                                    start_chunk,
                                    ..
                                } => {
                                    self.buffer.push(ResponseChunk::Leaf {
                                        size,
                                        is_root,
                                        start_chunk,
                                    });
                                }
                            }
                        }
                        self.buffer.reverse();
                    }
                }
            }
        }
    }
}

self_cell! {
    pub(crate) struct PreOrderChunkIterInner {
        owner: range_collections::RangeSet2<ChunkNum>,
        #[not_covariant]
        dependent: PreOrderChunkIterRef,
    }
}

impl PreOrderChunkIterInner {
    fn next(&mut self) -> Option<BaoChunk<&RangeSetRef<ChunkNum>>> {
        self.with_dependent_mut(|_, iter| iter.next())
    }

    fn tree(&self) -> &BaoTree {
        self.with_dependent(|_, iter| iter.tree())
    }
}

/// An iterator that produces chunks in pre order
pub struct PreOrderChunkIter(PreOrderChunkIterInner);

impl fmt::Debug for PreOrderChunkIter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreOrderChunkIter").finish_non_exhaustive()
    }
}

impl PreOrderChunkIter {
    /// Create a new iterator over the tree.
    pub fn new(tree: BaoTree, ranges: RangeSet2<ChunkNum>) -> Self {
        Self(PreOrderChunkIterInner::new(ranges, |ranges| {
            PreOrderChunkIterRef::new(tree, ranges, 0)
        }))
    }

    /// The tree this iterator is iterating over.
    pub fn tree(&self) -> &BaoTree {
        self.0.tree()
    }
}

impl Iterator for PreOrderChunkIter {
    type Item = BaoChunk;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|x| x.map_ranges(|_| ()))
    }
}
