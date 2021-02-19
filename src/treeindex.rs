extern crate crossbeam_epoch;

pub mod error;
pub mod leaf;
pub mod leafnode;
pub mod node;

use crossbeam_epoch::{Atomic, Guard, Owned};
use error::{InsertError, RemoveError, SearchError};
use leaf::{Leaf, LeafScanner};
use node::Node;
use std::fmt;
use std::sync::atomic::Ordering::{Acquire, Relaxed};

/// A scalable concurrent tree map implementation.
///
/// scc::TreeIndex is a B+ tree variant that is optimized for read operations.
/// Read operations, such as scan, read, are neither blocked nor interrupted by all the other types of operations.
/// Write operations, such as insert, remove, do not block if they do not entail structural changes to the tree.
pub struct TreeIndex<K, V>
where
    K: Clone + Ord + Send + Sync,
    V: Clone + Send + Sync,
{
    root: Atomic<Node<K, V>>,
}

impl<K, V> Default for TreeIndex<K, V>
where
    K: Clone + Ord + Send + Sync,
    V: Clone + Send + Sync,
{
    /// Creates a TreeIndex instance with the default parameters.
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = Default::default();
    ///
    /// let result = treeindex.read(&1, |key, value| *value);
    /// assert!(result.is_none());
    /// ```
    fn default() -> Self {
        TreeIndex::new()
    }
}

impl<K, V> TreeIndex<K, V>
where
    K: Clone + Ord + Send + Sync,
    V: Clone + Send + Sync,
{
    /// Creates an empty TreeIndex instance.
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// let result = treeindex.read(&1, |key, value| *value);
    /// assert!(result.is_none());
    /// ```
    pub fn new() -> TreeIndex<K, V> {
        TreeIndex {
            root: Atomic::null(),
        }
    }

    /// Inserts a a key-value pair.
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// let result = treeindex.insert(1, 10);
    /// assert!(result.is_ok());
    ///
    /// let result = treeindex.insert(1, 11);
    /// assert_eq!(result.err().unwrap(), (1, 11));
    ///
    /// let result = treeindex.read(&1, |key, value| *value);
    /// assert_eq!(result.unwrap(), 10);
    /// ```
    pub fn insert(&self, mut key: K, mut value: V) -> Result<(), (K, V)> {
        loop {
            let guard = crossbeam_epoch::pin();
            let mut root_node = self.root.load(Acquire, &guard);
            if root_node.is_null() {
                let new_root = Owned::new(Node::new(0, true));
                match self
                    .root
                    .compare_and_set(root_node, new_root, Relaxed, &guard)
                {
                    Ok(new_root) => root_node = new_root,
                    Err(_) => continue,
                }
            }
            let root_node_ref = unsafe { root_node.deref() };
            match root_node_ref.insert(key, value, &guard) {
                Ok(_) => return Ok(()),
                Err(error) => match error {
                    InsertError::Duplicated(entry) => return Err(entry),
                    InsertError::Full(entry) => {
                        root_node_ref.split_root(&self.root, &guard);
                        key = entry.0;
                        value = entry.1;
                    }
                    InsertError::Retry(entry) => {
                        key = entry.0;
                        value = entry.1;
                    }
                },
            }
        }
    }

    /// Removes a key-value pair.
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// let result = treeindex.remove(&1);
    /// assert!(!result);
    ///
    /// let result = treeindex.insert(1, 10);
    /// assert!(result.is_ok());
    ///
    /// let result = treeindex.remove(&1);
    /// assert!(result);
    /// ```
    pub fn remove(&self, key: &K) -> bool {
        let mut has_been_removed = false;
        let guard = crossbeam_epoch::pin();
        let mut root_node = self.root.load(Acquire, &guard);
        loop {
            if root_node.is_null() {
                return has_been_removed;
            }
            let root_node_ref = unsafe { root_node.deref() };
            match root_node_ref.remove(key, &guard) {
                Ok(removed) => return removed || has_been_removed,
                Err(remove_error) => match remove_error {
                    RemoveError::Coalesce(removed) => {
                        if removed && !has_been_removed {
                            has_been_removed = true;
                        }
                        Node::update_root(root_node, &self.root, &guard);
                        return has_been_removed;
                    }
                    RemoveError::Retry(removed) => {
                        if removed && !has_been_removed {
                            has_been_removed = true;
                        }
                    }
                },
            };
            let root_node_new = self.root.load(Acquire, &guard);
            root_node = root_node_new;
        }
    }

    /// Reads a key-value pair.
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// let result = treeindex.read(&1, |key, value| *value);
    /// assert!(result.is_none());
    ///
    /// let result = treeindex.insert(1, 10);
    /// assert!(result.is_ok());
    ///
    /// let result = treeindex.read(&1, |key, value| *value);
    /// assert_eq!(result.unwrap(), 10);
    /// ```
    pub fn read<U, F: FnOnce(&K, &V) -> U>(&self, key: &K, f: F) -> Option<U> {
        let guard = crossbeam_epoch::pin();
        loop {
            let root_node = self.root.load(Acquire, &guard);
            if root_node.is_null() {
                return None;
            }
            match unsafe { root_node.deref().search(key, &guard) } {
                Ok(result) => {
                    if let Some(value) = result {
                        return Some(f(key, value));
                    } else {
                        return None;
                    }
                }
                Err(err) => match err {
                    SearchError::Retry => continue,
                },
            }
        }
    }

    /// Clears the TreeIndex.
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// for key in 0..16u64 {
    ///     let result = treeindex.insert(key, 10);
    ///     assert!(result.is_ok());
    /// }
    ///
    /// treeindex.clear();
    ///
    /// let result = treeindex.len();
    /// assert_eq!(result, 0);
    /// ```
    pub fn clear(&self) {
        let guard = crossbeam_epoch::pin();
        Node::remove_root(&self.root, &guard);
    }

    /// Returns the size of the TreeIndex.
    ///
    /// It internally scans all the leaf nodes, and therefore the time complexity is O(N).
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// for key in 0..16u64 {
    ///     let result = treeindex.insert(key, 10);
    ///     assert!(result.is_ok());
    /// }
    ///
    /// let result = treeindex.len();
    /// assert_eq!(result, 16);
    /// ```
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Returns the depth of the TreeIndex.
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// for key in 0..16u64 {
    ///     let result = treeindex.insert(key, 10);
    ///     assert!(result.is_ok());
    /// }
    ///
    /// let result = treeindex.depth();
    /// assert_eq!(result, 1);
    /// ```
    pub fn depth(&self) -> usize {
        let guard = crossbeam_epoch::pin();
        let root_node = self.root.load(Acquire, &guard);
        if !root_node.is_null() {
            unsafe { root_node.deref().floor() + 1 }
        } else {
            0
        }
    }

    /// Returns a Scanner.
    ///
    /// The returned Scanner starts scanning from the minimum key-value pair.
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// let result = treeindex.insert(1, 10);
    /// assert!(result.is_ok());
    ///
    /// let result = treeindex.insert(2, 11);
    /// assert!(result.is_ok());
    ///
    /// let result = treeindex.insert(3, 13);
    /// assert!(result.is_ok());
    ///
    /// let mut scanner = treeindex.iter();
    /// assert_eq!(scanner.next().unwrap(), (&1, &10));
    /// assert_eq!(scanner.next().unwrap(), (&2, &11));
    /// assert_eq!(scanner.next().unwrap(), (&3, &13));
    /// assert!(scanner.next().is_none());
    /// ```
    pub fn iter(&self) -> Scanner<K, V> {
        Scanner::new(self)
    }

    /// Returns a Scanner that starts from the given key if it exists.
    ///
    /// In case the key does not exist, and there is a key that is greater than the given key,
    /// a Scanner pointing to the key is returned.
    ///
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// let result = treeindex.insert(1, 10);
    /// assert!(result.is_ok());
    ///
    /// let result = treeindex.insert(2, 11);
    /// assert!(result.is_ok());
    ///
    /// let result = treeindex.insert(3, 13);
    /// assert!(result.is_ok());
    ///
    /// if let Some(mut scanner) = treeindex.from(&2) {
    ///     assert_eq!(scanner.get().unwrap(), (&2, &11));
    ///     assert_eq!(scanner.next().unwrap(), (&3, &13));
    ///     assert!(scanner.next().is_none());
    /// }
    /// ```
    pub fn from(&self, key: &K) -> Option<Scanner<K, V>> {
        Scanner::from(self, key)
    }
}

impl<K, V> TreeIndex<K, V>
where
    K: Clone + fmt::Display + Ord + Send + Sync,
    V: Clone + fmt::Display + Send + Sync,
{
    /// Prints the TreeIndex contents to the given output in the DOT language.
    ///
    /// # Examples
    /// ```
    /// use scc::TreeIndex;
    ///
    /// let treeindex: TreeIndex<u64, u32> = TreeIndex::new();
    ///
    /// let result = treeindex.insert(1, 10);
    /// assert!(result.is_ok());
    ///
    /// treeindex.print(&mut std::io::stdout());
    /// ```
    pub fn print<T: std::io::Write>(&self, output: &mut T) -> std::io::Result<()> {
        output.write_fmt(format_args!("digraph {{\n"))?;
        let guard = crossbeam_epoch::pin();
        let root_node = self.root.load(Acquire, &guard);
        if !root_node.is_null() {
            unsafe { root_node.deref().print(output, &guard) }?
        }
        output.write_fmt(format_args!("}}"))
    }
}

impl<K, V> Drop for TreeIndex<K, V>
where
    K: Clone + Ord + Send + Sync,
    V: Clone + Send + Sync,
{
    fn drop(&mut self) {
        self.clear();
    }
}

/// Scanner implements Iterator for TreeIndex.
pub struct Scanner<'a, K, V>
where
    K: Clone + Ord + Send + Sync,
    V: Clone + Send + Sync,
{
    tree: &'a TreeIndex<K, V>,
    leaf_scanner: Option<LeafScanner<'a, K, V>>,
    guard: Guard,
}

impl<'a, K: Clone + Ord + Send + Sync, V: Clone + Send + Sync> Scanner<'a, K, V> {
    fn new(tree: &'a TreeIndex<K, V>) -> Scanner<'a, K, V> {
        Scanner::<'a, K, V> {
            tree,
            leaf_scanner: None,
            guard: crossbeam_epoch::pin(),
        }
    }

    fn from(tree: &'a TreeIndex<K, V>, min_allowed_key: &K) -> Option<Scanner<'a, K, V>> {
        let mut scanner = Scanner::<'a, K, V> {
            tree,
            leaf_scanner: None,
            guard: crossbeam_epoch::pin(),
        };
        loop {
            let root_node = tree.root.load(Acquire, &scanner.guard);
            if root_node.is_null() {
                return None;
            }
            if let Ok(leaf_scanner) =
                unsafe { &*root_node.as_raw() }.max_less(min_allowed_key, &scanner.guard)
            {
                scanner.leaf_scanner.replace(unsafe {
                    // Prolongs the lifetime as the rust type system cannot infer the actual lifetime correctly.
                    std::mem::transmute::<_, LeafScanner<'a, K, V>>(leaf_scanner)
                });
                break;
            }
        }

        while let Some((key_ref, _)) = scanner.next() {
            if key_ref.cmp(min_allowed_key) != std::cmp::Ordering::Less {
                return Some(scanner);
            }
        }
        None
    }

    /// Returns a reference to the entry that the scanner is currently pointing to.
    pub fn get(&self) -> Option<(&'a K, &'a V)> {
        if let Some(leaf_scanner) = self.leaf_scanner.as_ref() {
            return leaf_scanner.get();
        }
        None
    }
}

impl<'a, K, V> Iterator for Scanner<'a, K, V>
where
    K: Clone + Ord + Send + Sync,
    V: Clone + Send + Sync,
{
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        if self.leaf_scanner.is_none() {
            loop {
                let root_node = self.tree.root.load(Acquire, &self.guard);
                if root_node.is_null() {
                    return None;
                }
                if let Ok(leaf_scanner) = unsafe { &*root_node.as_raw() }.min(&self.guard) {
                    self.leaf_scanner.replace(unsafe {
                        // Prolongs the lifetime as the rust type system cannot infer the actual lifetime correctly.
                        std::mem::transmute::<_, LeafScanner<'a, K, V>>(leaf_scanner)
                    });
                    break;
                }
            }
        }
        if let Some(mut scanner) = self.leaf_scanner.take() {
            let min_allowed_key = scanner.get().map(|(key, _)| key);
            if let Some(result) = scanner.next() {
                self.leaf_scanner.replace(scanner);
                return Some(result);
            }
            // Proceeds to the next leaf node.
            while let Some(mut new_scanner) = unsafe {
                // Checking min_allowed_key is necessary, because,
                //  - Remove: merges two leaf nodes, therefore the unbounded leaf of the lower key node is relocated.
                //  - Scanner: the unbounded leaf of the lower key node can be scanned twice after a jump.
                std::mem::transmute::<_, Option<LeafScanner<'a, K, V>>>(scanner.jump(&self.guard))
                    .take()
            } {
                while let Some(entry) = new_scanner.next() {
                    if min_allowed_key
                        .as_ref()
                        .map_or_else(|| true, |&key| key.cmp(entry.0) == std::cmp::Ordering::Less)
                    {
                        self.leaf_scanner.replace(new_scanner);
                        return Some(entry);
                    }
                }
                scanner = new_scanner;
            }
        }
        None
    }
}
