// SPDX-License-Identifier: Apache-2.0

//! Pure algebra over a per-agent binary split tree (no daemon state / I/O).

use clowder_proto::{Axis, PaneId, PaneTree, SplitDirection, SplitId};

/// All pane ids, left-to-right / top-to-bottom.
#[allow(dead_code)]
pub(crate) fn leaves(tree: &PaneTree) -> Vec<PaneId> {
    match tree {
        PaneTree::Leaf { pane } => vec![*pane],
        PaneTree::Split { first, second, .. } => {
            let mut v = leaves(first);
            v.extend(leaves(second));
            v
        }
    }
}

#[allow(dead_code)]
pub(crate) fn contains(tree: &PaneTree, pane: PaneId) -> bool {
    leaves(tree).contains(&pane)
}

/// Replace `Leaf(target)` with a fresh `Split` of `target` (first) and `companion` (second).
/// `Right` → Horizontal, `Down` → Vertical. Returns false if `target` isn't a leaf here.
#[allow(dead_code)]
pub(crate) fn split_leaf(
    tree: &mut PaneTree,
    target: PaneId,
    companion: PaneId,
    direction: SplitDirection,
    id: SplitId,
) -> bool {
    match tree {
        PaneTree::Leaf { pane } if *pane == target => {
            let axis = match direction {
                SplitDirection::Right => Axis::Horizontal,
                SplitDirection::Down => Axis::Vertical,
            };
            *tree = PaneTree::Split {
                id,
                axis,
                ratio: 0.5,
                first: Box::new(PaneTree::Leaf { pane: target }),
                second: Box::new(PaneTree::Leaf { pane: companion }),
            };
            true
        }
        PaneTree::Leaf { .. } => false,
        PaneTree::Split { first, second, .. } => {
            split_leaf(first, target, companion, direction, id)
                || split_leaf(second, target, companion, direction, id)
        }
    }
}

/// Remove `Leaf(pane)`, collapsing its parent split by promoting the sibling. Returns false
/// if `pane` is absent or is the tree's sole leaf.
#[allow(dead_code)]
pub(crate) fn remove_leaf(tree: &mut PaneTree, pane: PaneId) -> bool {
    match tree {
        PaneTree::Leaf { .. } => false, // a lone leaf cannot remove itself
        PaneTree::Split { first, second, .. } => {
            let first_is_target = matches!(first.as_ref(), PaneTree::Leaf { pane: p } if *p == pane);
            let second_is_target = matches!(second.as_ref(), PaneTree::Leaf { pane: p } if *p == pane);
            if first_is_target {
                let sibling = std::mem::replace(second.as_mut(), PaneTree::Leaf { pane });
                *tree = sibling;
                true
            } else if second_is_target {
                let sibling = std::mem::replace(first.as_mut(), PaneTree::Leaf { pane });
                *tree = sibling;
                true
            } else {
                remove_leaf(first, pane) || remove_leaf(second, pane)
            }
        }
    }
}

/// Set the divider ratio (clamped to [0.05, 0.95]) on the split with `id`.
#[allow(dead_code)]
pub(crate) fn set_ratio(tree: &mut PaneTree, id: SplitId, ratio: f32) -> bool {
    match tree {
        PaneTree::Leaf { .. } => false,
        PaneTree::Split { id: sid, ratio: r, first, second, .. } => {
            if *sid == id {
                *r = ratio.clamp(0.05, 0.95);
                true
            } else {
                set_ratio(first, id, ratio) || set_ratio(second, id, ratio)
            }
        }
    }
}

/// Rebuild a persisted tree for restore: keep the agent leaf (its id is stable across restart),
/// spawn a fresh companion for every other leaf (substituting the new id), regenerate split ids,
/// and preserve axis + ratio. Best-effort: if `spawn_companion` returns None, that leaf collapses
/// into its sibling. Returns the rebuilt tree and the new companion ids in creation order; always
/// yields at least `Leaf { pane: agent }`.
pub(crate) fn rebuild_for_restore(
    tree: &PaneTree,
    agent: PaneId,
    spawn_companion: &mut dyn FnMut() -> Option<PaneId>,
    alloc_split: &mut dyn FnMut() -> SplitId,
) -> (PaneTree, Vec<PaneId>) {
    rebuild(tree, agent, spawn_companion, alloc_split)
        .unwrap_or_else(|| (PaneTree::Leaf { pane: agent }, Vec::new()))
}

/// Recursion for `rebuild_for_restore`. `None` = this subtree produced no panes (fully collapsed);
/// the agent leaf can never collapse, so the top-level call always yields `Some`.
fn rebuild(
    node: &PaneTree,
    agent: PaneId,
    spawn_companion: &mut dyn FnMut() -> Option<PaneId>,
    alloc_split: &mut dyn FnMut() -> SplitId,
) -> Option<(PaneTree, Vec<PaneId>)> {
    match node {
        PaneTree::Leaf { pane } if *pane == agent => {
            Some((PaneTree::Leaf { pane: agent }, Vec::new()))
        }
        PaneTree::Leaf { .. } => {
            let id = spawn_companion()?; // None → collapse
            Some((PaneTree::Leaf { pane: id }, vec![id]))
        }
        PaneTree::Split { axis, ratio, first, second, .. } => {
            let f = rebuild(first, agent, spawn_companion, alloc_split);
            let s = rebuild(second, agent, spawn_companion, alloc_split);
            match (f, s) {
                (Some((ft, mut fc)), Some((st, sc))) => {
                    fc.extend(sc);
                    Some((
                        PaneTree::Split {
                            id: alloc_split(),
                            axis: *axis,
                            ratio: *ratio,
                            first: Box::new(ft),
                            second: Box::new(st),
                        },
                        fc,
                    ))
                }
                // one side collapsed → promote the surviving side (drop the divider)
                (Some(x), None) | (None, Some(x)) => Some(x),
                (None, None) => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clowder_proto::{PaneId, SplitDirection, SplitId};

    fn leaf(n: u64) -> PaneTree { PaneTree::Leaf { pane: PaneId(n) } }

    #[test]
    fn split_a_leaf_makes_a_binary_split() {
        let mut t = leaf(1);
        assert!(split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1)));
        match &t {
            PaneTree::Split { axis, first, second, .. } => {
                assert_eq!(*axis, Axis::Horizontal);
                assert_eq!(**first, leaf(1));
                assert_eq!(**second, leaf(2));
            }
            _ => panic!("expected split"),
        }
        assert_eq!(leaves(&t), vec![PaneId(1), PaneId(2)]);
    }

    #[test]
    fn split_down_is_vertical_and_nests() {
        let mut t = leaf(1);
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        // split the companion (pane 2) downward
        assert!(split_leaf(&mut t, PaneId(2), PaneId(3), SplitDirection::Down, SplitId(2)));
        assert_eq!(leaves(&t), vec![PaneId(1), PaneId(2), PaneId(3)]);
        // the second child is now a vertical split of 2 and 3
        if let PaneTree::Split { second, .. } = &t {
            assert!(matches!(**second, PaneTree::Split { axis: Axis::Vertical, .. }));
        } else { panic!() }
    }

    #[test]
    fn split_unknown_target_is_false() {
        let mut t = leaf(1);
        assert!(!split_leaf(&mut t, PaneId(9), PaneId(2), SplitDirection::Right, SplitId(1)));
    }

    #[test]
    fn remove_collapses_parent_to_sibling() {
        let mut t = leaf(1);
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        assert!(remove_leaf(&mut t, PaneId(2)));
        assert_eq!(t, leaf(1)); // collapsed back to a lone leaf
    }

    #[test]
    fn remove_in_nested_promotes_sibling() {
        let mut t = leaf(1);
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        split_leaf(&mut t, PaneId(2), PaneId(3), SplitDirection::Down, SplitId(2));
        assert!(remove_leaf(&mut t, PaneId(3)));
        assert_eq!(leaves(&t), vec![PaneId(1), PaneId(2)]);
    }

    #[test]
    fn remove_last_or_absent_is_false() {
        let mut t = leaf(1);
        assert!(!remove_leaf(&mut t, PaneId(1))); // sole leaf can't be removed
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        assert!(!remove_leaf(&mut t, PaneId(9))); // absent
    }

    #[test]
    fn set_ratio_finds_and_clamps() {
        let mut t = leaf(1);
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        assert!(set_ratio(&mut t, SplitId(1), 2.0)); // clamps
        if let PaneTree::Split { ratio, .. } = &t { assert_eq!(*ratio, 0.95); } else { panic!() }
        assert!(!set_ratio(&mut t, SplitId(9), 0.5)); // unknown id
    }

    // A scripted spawner: returns the next id from `ids`, or None once exhausted / where scripted.
    fn spawner(ids: Vec<Option<u64>>) -> impl FnMut() -> Option<PaneId> {
        let mut it = ids.into_iter();
        move || it.next().flatten().map(PaneId)
    }
    fn splitter() -> impl FnMut() -> SplitId {
        let mut n = 1000u64;
        move || { n += 1; SplitId(n) }
    }

    #[test]
    fn rebuild_agent_only_leaf_is_single_leaf() {
        let t = leaf(1);
        let mut sp = spawner(vec![]);
        let mut al = splitter();
        let (out, comps) = rebuild_for_restore(&t, PaneId(1), &mut sp, &mut al);
        assert_eq!(out, leaf(1));
        assert!(comps.is_empty());
    }

    #[test]
    fn rebuild_substitutes_companion_and_preserves_axis_ratio() {
        // agent=1, one companion leaf (old id 77) under a horizontal split at ratio 0.3.
        let t = PaneTree::Split {
            id: SplitId(5), axis: Axis::Horizontal, ratio: 0.3,
            first: Box::new(leaf(1)), second: Box::new(leaf(77)),
        };
        let mut sp = spawner(vec![Some(500)]);
        let mut al = splitter();
        let (out, comps) = rebuild_for_restore(&t, PaneId(1), &mut sp, &mut al);
        assert_eq!(comps, vec![PaneId(500)]);
        match out {
            PaneTree::Split { id, axis, ratio, first, second } => {
                assert_eq!(id, SplitId(1001));            // regenerated, not the old 5
                assert_eq!(axis, Axis::Horizontal);
                assert_eq!(ratio, 0.3);
                assert_eq!(*first, leaf(1));              // agent leaf preserved
                assert_eq!(*second, leaf(500));           // companion substituted
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn rebuild_failed_companion_collapses_to_agent() {
        let t = PaneTree::Split {
            id: SplitId(5), axis: Axis::Vertical, ratio: 0.5,
            first: Box::new(leaf(1)), second: Box::new(leaf(77)),
        };
        let mut sp = spawner(vec![None]);   // the companion spawn fails
        let mut al = splitter();
        let (out, comps) = rebuild_for_restore(&t, PaneId(1), &mut sp, &mut al);
        assert_eq!(out, leaf(1));            // collapsed to the surviving agent leaf
        assert!(comps.is_empty());
    }

    #[test]
    fn rebuild_nested_recurses_and_one_failure_collapses_inner() {
        // agent=1 ; right side is a split of two companions (88, 99); 88 fails, 99 succeeds.
        let t = PaneTree::Split {
            id: SplitId(5), axis: Axis::Horizontal, ratio: 0.6,
            first: Box::new(leaf(1)),
            second: Box::new(PaneTree::Split {
                id: SplitId(6), axis: Axis::Vertical, ratio: 0.2,
                first: Box::new(leaf(88)), second: Box::new(leaf(99)),
            }),
        };
        let mut sp = spawner(vec![None, Some(501)]);  // 88 → None, 99 → 501
        let mut al = splitter();
        let (out, comps) = rebuild_for_restore(&t, PaneId(1), &mut sp, &mut al);
        assert_eq!(comps, vec![PaneId(501)]);
        // inner split collapsed to leaf(501); outer split keeps agent + that leaf.
        match out {
            PaneTree::Split { axis, ratio, first, second, .. } => {
                assert_eq!(axis, Axis::Horizontal);
                assert_eq!(ratio, 0.6);
                assert_eq!(*first, leaf(1));
                assert_eq!(*second, leaf(501));
            }
            _ => panic!("expected split"),
        }
    }
}
