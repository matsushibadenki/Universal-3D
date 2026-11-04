//! CPU bounds used by snapping. Kept independent from the document and GPU buffers.
use std::collections::HashMap;
use std::hash::Hash;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}
impl Bounds {
    pub fn point(p: [f32; 3]) -> Self {
        Self { min: p, max: p }
    }
    pub fn union(self, other: Self) -> Self {
        Self {
            min: std::array::from_fn(|i| self.min[i].min(other.min[i])),
            max: std::array::from_fn(|i| self.max[i].max(other.max[i])),
        }
    }
    fn area(self) -> f64 {
        let d: [f64; 3] = std::array::from_fn(|i| f64::from(self.max[i]) - f64::from(self.min[i]));
        d[0] * d[1] + d[1] * d[2] + d[2] * d[0]
    }
    pub fn transformed(self, matrix: [[f32; 4]; 4]) -> Self {
        // Interval arithmetic in f64 plus an outward margin covers f32 model-point rounding.
        let mut result = self;
        for i in 0..3 {
            let mut lo = f64::from(matrix[3][i]);
            let mut hi = lo;
            for (j, column) in matrix.iter().enumerate().take(3) {
                let a = f64::from(column[i]) * f64::from(self.min[j]);
                let b = f64::from(column[i]) * f64::from(self.max[j]);
                lo += a.min(b);
                hi += a.max(b);
            }
            let margin = (lo.abs().max(hi.abs()) + 1.) * 0.000002;
            result.min[i] = (lo - margin) as f32;
            result.max[i] = (hi + margin) as f32;
        }
        result
    }
    pub fn intersects(self, planes: &[[f64; 4]; 6]) -> bool {
        planes.iter().all(|p| {
            let mut support = p[3];
            let mut magnitude = p[3].abs() + 1.;
            for (i, coefficient) in p.iter().enumerate().take(3) {
                let v = coefficient
                    * f64::from(if *coefficient >= 0. {
                        self.max[i]
                    } else {
                        self.min[i]
                    });
                support += v;
                magnitude += v.abs();
            }
            support >= -magnitude * 0.000002
        })
    }
}
#[derive(Clone)]
struct Node<K> {
    bounds: Bounds,
    parent: Option<usize>,
    children: Option<[usize; 2]>,
    key: Option<K>,
    height: i32,
}
// Height-balanced dynamic bounds tree. Only changed leaves and ancestors are repaired.
#[derive(Clone)]
pub struct BoundsTree<K> {
    nodes: Vec<Node<K>>,
    free: Vec<usize>,
    root: Option<usize>,
    leaves: HashMap<K, usize>,
}
impl<K> Default for BoundsTree<K> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            free: Vec::new(),
            root: None,
            leaves: HashMap::new(),
        }
    }
}
impl<K: Copy + Eq + Hash> BoundsTree<K> {
    /// Balanced initial tree; subsequent edits continue to use local insert/remove repairs.
    pub fn from_leaves(leaves: Vec<(K, Bounds)>) -> Self {
        Self::from_leaves_cancellable(leaves, None).expect("Uncancelled bounds construction")
    }
    pub fn from_leaves_cancellable(
        mut leaves: Vec<(K, Bounds)>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Self, String> {
        crate::snapping::check_cancel(cancel)?;
        let mut tree = Self {
            nodes: Vec::with_capacity(leaves.len().saturating_mul(2).saturating_sub(1)),
            leaves: HashMap::with_capacity(leaves.len()),
            ..Self::default()
        };
        if !leaves.is_empty() {
            tree.root = Some(tree.build_leaves(&mut leaves, None, cancel)?);
        }
        crate::snapping::check_cancel(cancel)?;
        Ok(tree)
    }
    fn build_leaves(
        &mut self,
        leaves: &mut [(K, Bounds)],
        parent: Option<usize>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<usize, String> {
        if self.nodes.len().is_multiple_of(1024) {
            crate::snapping::check_cancel(cancel)?;
        }
        let bounds = leaves
            .iter()
            .map(|(_, b)| *b)
            .reduce(Bounds::union)
            .unwrap();
        let index = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            parent,
            children: None,
            key: None,
            height: 0,
        });
        if leaves.len() == 1 {
            let key = leaves[0].0;
            self.nodes[index].key = Some(key);
            assert!(
                self.leaves.insert(key, index).is_none(),
                "Duplicate bounds-tree key"
            );
        } else {
            let axis = (0..3)
                .max_by(|&a, &b| {
                    (bounds.max[a] - bounds.min[a]).total_cmp(&(bounds.max[b] - bounds.min[b]))
                })
                .unwrap();
            let middle = leaves.len() / 2;
            leaves.select_nth_unstable_by(middle, |(_, a), (_, b)| {
                let center = |v: &Bounds| v.min[axis] * 0.5 + v.max[axis] * 0.5;
                center(a).total_cmp(&center(b))
            });
            let (left, right) = leaves.split_at_mut(middle);
            let a = self.build_leaves(left, Some(index), cancel)?;
            let b = self.build_leaves(right, Some(index), cancel)?;
            self.nodes[index].children = Some([a, b]);
            self.refresh(index);
        }
        Ok(index)
    }
    fn allocate(&mut self, node: Node<K>) -> usize {
        if let Some(i) = self.free.pop() {
            self.nodes[i] = node;
            i
        } else {
            self.nodes.push(node);
            self.nodes.len() - 1
        }
    }
    fn release(&mut self, i: usize) {
        self.nodes[i].key = None;
        self.nodes[i].children = None;
        self.nodes[i].parent = None;
        self.free.push(i);
    }
    fn refresh(&mut self, i: usize) {
        if let Some([a, b]) = self.nodes[i].children {
            self.nodes[i].bounds = self.nodes[a].bounds.union(self.nodes[b].bounds);
            self.nodes[i].height = 1 + self.nodes[a].height.max(self.nodes[b].height);
        }
    }
    fn replace_child(&mut self, parent: Option<usize>, old: usize, new: usize) {
        if let Some(p) = parent {
            let children = self.nodes[p].children.as_mut().unwrap();
            if children[0] == old {
                children[0] = new;
            } else {
                debug_assert_eq!(children[1], old);
                children[1] = new;
            }
        } else {
            self.root = Some(new);
        }
        self.nodes[new].parent = parent;
    }
    fn rotate_left(&mut self, i: usize) -> usize {
        let [a, b] = self.nodes[i].children.unwrap();
        let [c, d] = self.nodes[b].children.unwrap();
        let parent = self.nodes[i].parent;
        self.replace_child(parent, i, b);
        self.nodes[b].children = Some([i, d]);
        self.nodes[i].parent = Some(b);
        self.nodes[i].children = Some([a, c]);
        self.nodes[c].parent = Some(i);
        self.refresh(i);
        self.refresh(b);
        b
    }
    fn rotate_right(&mut self, i: usize) -> usize {
        let [a, b] = self.nodes[i].children.unwrap();
        let [c, d] = self.nodes[a].children.unwrap();
        let parent = self.nodes[i].parent;
        self.replace_child(parent, i, a);
        self.nodes[a].children = Some([c, i]);
        self.nodes[i].parent = Some(a);
        self.nodes[i].children = Some([d, b]);
        self.nodes[d].parent = Some(i);
        self.refresh(i);
        self.refresh(a);
        a
    }
    fn repair(&mut self, mut current: Option<usize>) {
        while let Some(i) = current {
            self.refresh(i);
            let mut top = i;
            if let Some([a, b]) = self.nodes[i].children {
                let balance = self.nodes[a].height - self.nodes[b].height;
                if balance > 1 {
                    let [c, d] = self.nodes[a].children.unwrap();
                    if self.nodes[c].height < self.nodes[d].height {
                        self.rotate_left(a);
                    }
                    top = self.rotate_right(i);
                } else if balance < -1 {
                    let [c, d] = self.nodes[b].children.unwrap();
                    if self.nodes[c].height > self.nodes[d].height {
                        self.rotate_right(b);
                    }
                    top = self.rotate_left(i);
                }
            }
            current = self.nodes[top].parent;
        }
    }
    pub fn remove(&mut self, key: K) {
        let Some(leaf) = self.leaves.remove(&key) else {
            return;
        };
        if let Some(parent) = self.nodes[leaf].parent {
            let children = self.nodes[parent].children.unwrap();
            let sibling = if children[0] == leaf {
                children[1]
            } else {
                children[0]
            };
            let grandparent = self.nodes[parent].parent;
            self.replace_child(grandparent, parent, sibling);
            self.release(parent);
            self.release(leaf);
            self.repair(grandparent);
        } else {
            self.root = None;
            self.release(leaf);
        }
    }
    pub fn insert(&mut self, key: K, bounds: Bounds) {
        self.remove(key);
        let leaf = self.allocate(Node {
            bounds,
            parent: None,
            children: None,
            key: Some(key),
            height: 0,
        });
        self.leaves.insert(key, leaf);
        let Some(mut sibling) = self.root else {
            self.root = Some(leaf);
            return;
        };
        while let Some([a, b]) = self.nodes[sibling].children {
            let cost =
                |i: usize| self.nodes[i].bounds.union(bounds).area() - self.nodes[i].bounds.area();
            sibling = if cost(a) <= cost(b) { a } else { b };
        }
        let old_parent = self.nodes[sibling].parent;
        let parent = self.allocate(Node {
            bounds: self.nodes[sibling].bounds.union(bounds),
            parent: old_parent,
            children: Some([sibling, leaf]),
            key: None,
            height: 1,
        });
        self.replace_child(old_parent, sibling, parent);
        self.nodes[sibling].parent = Some(parent);
        self.nodes[leaf].parent = Some(parent);
        self.repair(Some(parent));
    }
    pub fn vec_bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node<K>>()
            + self.free.capacity() * std::mem::size_of::<usize>()
    }
    pub fn entry_payload_bytes(&self) -> usize {
        self.leaves.len() * std::mem::size_of::<(K, usize)>()
    }
    pub fn query(&self, planes: &[[f64; 4]; 6], mut visit: impl FnMut(K)) -> usize {
        let mut tested = 0;
        let mut stack = Vec::new();
        if let Some(root) = self.root {
            stack.push(root);
        }
        while let Some(i) = stack.pop() {
            tested += 1;
            let node = &self.nodes[i];
            if !node.bounds.intersects(planes) {
                continue;
            }
            if let Some(key) = node.key {
                visit(key);
            } else if let Some(children) = node.children {
                stack.extend(children);
            }
        }
        tested
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn box_planes(min: [f32; 3], max: [f32; 3]) -> [[f64; 4]; 6] {
        std::array::from_fn(|i| {
            let axis = i / 2;
            let sign = if i % 2 == 0 { 1. } else { -1. };
            let mut p = [0.; 4];
            p[axis] = sign;
            p[3] = if sign > 0. {
                -f64::from(min[axis])
            } else {
                f64::from(max[axis])
            };
            p
        })
    }
    fn check(tree: &BoundsTree<u64>, i: usize, parent: Option<usize>) -> (Bounds, i32, usize) {
        let n = &tree.nodes[i];
        assert_eq!(n.parent, parent);
        if let Some(key) = n.key {
            assert_eq!(tree.leaves[&key], i);
            assert_eq!(n.height, 0);
            return (n.bounds, 0, 1);
        }
        let [a, b] = n.children.unwrap();
        let (ba, ha, na) = check(tree, a, Some(i));
        let (bb, hb, nb) = check(tree, b, Some(i));
        assert!((ha - hb).abs() <= 1);
        assert_eq!(n.height, 1 + ha.max(hb));
        assert_eq!(n.bounds, ba.union(bb));
        (n.bounds, n.height, na + nb)
    }
    #[test]
    fn bulk_tree_matches_linear_search_and_remains_editable() {
        for count in [0, 1, 1024] {
            let mut oracle: HashMap<u64, Bounds> = (0..count)
                .map(|id| (id, Bounds::point([(id % 37) as f32, (id % 11) as f32, 0.])))
                .collect();
            let mut tree = BoundsTree::from_leaves(oracle.iter().map(|(k, b)| (*k, *b)).collect());
            for step in 0..300 {
                let id = step % 71;
                if step % 3 == 0 {
                    tree.remove(id);
                    oracle.remove(&id);
                } else {
                    let bounds = Bounds::point([step as f32 / 10., 3., 0.]);
                    tree.insert(id, bounds);
                    oracle.insert(id, bounds);
                }
                if let Some(root) = tree.root {
                    assert_eq!(check(&tree, root, None).2, oracle.len());
                } else {
                    assert!(oracle.is_empty());
                }
                let planes = box_planes([5., 2., -1.], [23., 8., 1.]);
                let mut found = Vec::new();
                tree.query(&planes, |k| found.push(k));
                found.sort();
                let mut expected: Vec<_> = oracle
                    .iter()
                    .filter(|(_, b)| b.intersects(&planes))
                    .map(|(k, _)| *k)
                    .collect();
                expected.sort();
                assert_eq!(found, expected);
            }
        }
        let coincident =
            BoundsTree::from_leaves((0..1000).map(|id| (id, Bounds::point([0.; 3]))).collect());
        assert_eq!(check(&coincident, coincident.root.unwrap(), None).2, 1000);
    }
    #[test]
    fn dynamic_tree_handles_sorted_inserts_moves_removals_and_slot_reuse() {
        let mut tree = BoundsTree::default();
        let mut oracle = HashMap::new();
        for step in 0..2500u64 {
            let id = step % 701;
            let p = [(step % 97) as f32, (step % 31) as f32, (step % 13) as f32];
            if step % 5 == 0 {
                tree.remove(id);
                oracle.remove(&id);
            } else {
                let bounds = Bounds::point(p);
                tree.insert(id, bounds);
                oracle.insert(id, bounds);
            }
            if step % 11 == 0 {
                if let Some(root) = tree.root {
                    let (_, _, count) = check(&tree, root, None);
                    assert_eq!(count, oracle.len());
                }
                let planes = box_planes([10., 5., 2.], [45., 20., 10.]);
                let mut found = Vec::new();
                tree.query(&planes, |key| found.push(key));
                found.sort();
                let mut expected: Vec<_> = oracle
                    .iter()
                    .filter(|(_, b)| b.intersects(&planes))
                    .map(|(id, _)| *id)
                    .collect();
                expected.sort();
                assert_eq!(found, expected);
            }
        }
        for id in 0..701 {
            tree.remove(id);
        }
        assert!(tree.root.is_none());
        assert_eq!(tree.free.len(), tree.nodes.len());
    }
}
