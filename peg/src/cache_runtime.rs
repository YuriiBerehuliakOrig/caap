//! Runtime cache helpers: overlay memo, bounded map, node-memo autodisable.
//!
//! Ported from `peg/engine/cache_runtime.py` and `peg/engine/cache_model.py`.

use crate::types::ParseValue;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::sync::Arc;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const NODE_MEMO_AUTODISABLE_MISS_THRESHOLD: usize = 256;
pub const NODE_MEMO_AUTODISABLE_HIT_RATIO_DENOM: usize = 128;
pub const SKIP_MEMO_LIMIT: usize = 65_536;

// ── Type aliases ──────────────────────────────────────────────────────────────

/// Rule-level packrat memo key: `(rule_name, start_pos)`.
pub type RuleMemoKey = (String, usize);

/// Node-level memo key: `(node_id, start_pos, epoch)`.
pub type NodeMemoKey = (u32, usize, u32);

// ── MemoEntry ─────────────────────────────────────────────────────────────────

/// A cached parse result: success with end-pos + value, or failure at a position.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoEntry {
    pub ok: bool,
    /// End position if `ok`, failure position if `!ok`.
    pub pos: usize,
    pub value: Option<ParseValue>,
}

impl MemoEntry {
    pub fn success(pos: usize, value: ParseValue) -> Self {
        Self {
            ok: true,
            pos,
            value: Some(value),
        }
    }

    pub fn failure(pos: usize) -> Self {
        Self {
            ok: false,
            pos,
            value: None,
        }
    }
}

// ── RuleMemoState / NodeMemoState ─────────────────────────────────────────────

/// Per-invocation memoization state for a rule call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleMemoState {
    pub key: RuleMemoKey,
    pub use_memo: bool,
}

impl RuleMemoState {
    pub fn new(name: impl Into<String>, pos: usize, use_memo: bool) -> Self {
        Self {
            key: (name.into(), pos),
            use_memo,
        }
    }
}

/// Per-invocation memoization state for a node evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeMemoState {
    pub key: NodeMemoKey,
    pub epoch: u32,
}

impl NodeMemoState {
    pub fn new(node_id: u32, pos: usize, epoch: u32) -> Self {
        Self {
            key: (node_id, pos, epoch),
            epoch,
        }
    }
}

// ── BoundedMap ────────────────────────────────────────────────────────────────

/// Hash map with an optional FIFO-eviction capacity limit.
///
/// When `capacity` is set and the map would exceed it, the oldest-inserted entry
/// is evicted.  The total eviction count is tracked in `evictions`.
pub struct BoundedMap<K: Eq + Hash + Clone, V> {
    map: HashMap<K, V>,
    order: VecDeque<K>,
    capacity: Option<usize>,
    pub evictions: usize,
}

impl<K: Eq + Hash + Clone, V> Default for BoundedMap<K, V> {
    fn default() -> Self {
        Self::unbounded()
    }
}

impl<K: Eq + Hash + Clone, V> BoundedMap<K, V> {
    pub fn unbounded() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity: None,
            evictions: 0,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity: Some(cap),
            evictions: 0,
        }
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let already_present = self.map.contains_key(&key);
        let old = self.map.insert(key.clone(), value);
        if !already_present {
            self.order.push_back(key);
        }
        if let Some(cap) = self.capacity {
            while self.map.len() > cap {
                // Pop stale order entries that were already removed from the map.
                while let Some(oldest) = self.order.pop_front() {
                    if self.map.remove(&oldest).is_some() {
                        self.evictions += 1;
                        break;
                    }
                }
            }
        }
        old
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.map.remove(key)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.map.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.map.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.map.values()
    }
}

// ── OverlayMemo ───────────────────────────────────────────────────────────────

/// Two-layer map: a read-only base (persisted) plus a bounded writable overlay.
///
/// Reads check the overlay first; writes go to the overlay.  Entries in the
/// overlay shadow entries with the same key in the base.  `materialize()` returns
/// a flat `HashMap` with overlay entries taking priority.
pub struct OverlayMemo<K: Eq + Hash + Clone, V: Clone> {
    base: Option<Arc<HashMap<K, V>>>,
    overlay: BoundedMap<K, V>,
}

impl<K: Eq + Hash + Clone, V: Clone> OverlayMemo<K, V> {
    /// Create a new overlay over `base` with an optional overlay capacity limit.
    pub fn new(base: Option<Arc<HashMap<K, V>>>, capacity: Option<usize>) -> Self {
        let overlay = match capacity {
            Some(cap) => BoundedMap::with_capacity(cap),
            None => BoundedMap::unbounded(),
        };
        Self { base, overlay }
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.overlay
            .get(key)
            .or_else(|| self.base.as_ref()?.get(key))
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.overlay.insert(key, value)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.overlay.contains_key(key) || self.base.as_ref().is_some_and(|b| b.contains_key(key))
    }

    /// Total distinct keys visible across both layers.
    pub fn len(&self) -> usize {
        match &self.base {
            None => self.overlay.len(),
            Some(base) => {
                let extra = base
                    .keys()
                    .filter(|k| !self.overlay.contains_key(k))
                    .count();
                self.overlay.len() + extra
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn evictions(&self) -> usize {
        self.overlay.evictions
    }

    /// Collect all visible entries into a new `HashMap` (overlay wins on conflict).
    pub fn materialize(&self) -> HashMap<K, V> {
        let mut out = self
            .base
            .as_ref()
            .map_or_else(HashMap::new, |b| (**b).clone());
        for (k, v) in self.overlay.iter() {
            out.insert(k.clone(), v.clone());
        }
        out
    }

    pub fn clear(&mut self) {
        self.overlay.clear();
        self.base = None;
    }
}

// ── NodeMemoOps ───────────────────────────────────────────────────────────────

/// Node memoization with windowed eviction and hit-ratio autodisable.
///
/// Owns the node memo table and all associated counters.  If the hit ratio
/// falls below the threshold after `miss_threshold` misses, the memo is cleared
/// and disabled for the rest of the parse run.
pub struct NodeMemoOps {
    // config
    miss_threshold: usize,
    hit_ratio_denom: usize,
    window: Option<usize>,
    limit: Option<usize>,
    prune_cadence: usize,
    // state
    pub memo: HashMap<NodeMemoKey, MemoEntry>,
    epoch_keys: HashMap<u32, HashSet<NodeMemoKey>>,
    pub hits: usize,
    pub misses: usize,
    pub evictions: usize,
    pub enabled: bool,
    prune_tick: usize,
}

impl Default for NodeMemoOps {
    fn default() -> Self {
        Self::new(
            NODE_MEMO_AUTODISABLE_MISS_THRESHOLD,
            NODE_MEMO_AUTODISABLE_HIT_RATIO_DENOM,
            None,
            None,
            None,
        )
    }
}

impl NodeMemoOps {
    pub fn new(
        miss_threshold: usize,
        hit_ratio_denom: usize,
        window: Option<usize>,
        limit: Option<usize>,
        prune_cadence: Option<usize>,
    ) -> Self {
        Self {
            miss_threshold,
            hit_ratio_denom,
            window,
            limit,
            prune_cadence: prune_cadence.unwrap_or(64),
            memo: HashMap::new(),
            epoch_keys: HashMap::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
            enabled: true,
            prune_tick: 0,
        }
    }

    /// Compute the `NodeMemoState` for a node at `pos`, taking epochs into account.
    ///
    /// Returns `None` when node memo is disabled.
    pub fn prepare_state(
        &self,
        node_id: u32,
        pos: usize,
        epoch_starts: &[usize],
        current_epoch: u32,
    ) -> Option<NodeMemoState> {
        if !self.enabled {
            return None;
        }
        let epoch = if epoch_starts.last().is_some_and(|&s| pos >= s) {
            current_epoch
        } else {
            0
        };
        Some(NodeMemoState::new(node_id, pos, epoch))
    }

    /// Look up a cached result.  Returns `None` on a miss (without incrementing `misses`).
    pub fn lookup(&mut self, state: &NodeMemoState) -> Option<&MemoEntry> {
        let entry = self.memo.get(&state.key)?;
        self.hits += 1;
        Some(entry)
    }

    /// Store a result and trigger pruning / autodisable heuristics.
    pub fn store(&mut self, state: &NodeMemoState, entry: MemoEntry, current_pos: usize) {
        self.misses += 1;
        if state.epoch > 0 {
            self.epoch_keys
                .entry(state.epoch)
                .or_default()
                .insert(state.key);
        }
        self.memo.insert(state.key, entry);
        self.prune_budget(current_pos, false);
        self.maybe_autodisable();
    }

    /// Evict all entries tagged with `epoch`.
    pub fn prune_epoch(&mut self, epoch: u32) {
        if epoch == 0 {
            return;
        }
        if let Some(stale) = self.epoch_keys.remove(&epoch) {
            for key in stale {
                if self.memo.remove(&key).is_some() {
                    self.evictions += 1;
                }
            }
        }
    }

    /// Window + limit eviction pass.  Skipped unless the cadence tick fires (or `force`).
    pub fn prune_budget(&mut self, current_pos: usize, force: bool) {
        if !self.enabled {
            return;
        }
        if !force {
            self.prune_tick += 1;
            if self.prune_tick < self.prune_cadence {
                return;
            }
        }
        self.prune_tick = 0;

        // Window eviction: drop entries whose position is older than the window.
        if let Some(window) = self.window {
            let cutoff = current_pos.saturating_sub(window);
            if cutoff > 0 {
                let stale: Vec<NodeMemoKey> =
                    self.memo.keys().filter(|k| k.1 < cutoff).copied().collect();
                for key in stale {
                    self.drop_key(key);
                }
            }
        }

        // Limit eviction: keep at most `limit` entries, evicting lowest-priority first.
        if let Some(limit) = self.limit {
            if self.memo.len() > limit {
                let excess = self.memo.len() - limit;
                // Prioritise evicting epoch-0 (non-epoched) entries first,
                // then entries at the lowest position.
                let mut ordered: Vec<NodeMemoKey> = self.memo.keys().copied().collect();
                ordered.sort_by_key(|k| (if k.2 > 0 { 0u8 } else { 1u8 }, k.1, k.0));
                for key in ordered.into_iter().take(excess) {
                    self.drop_key(key);
                }
            }
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn memo_len(&self) -> usize {
        self.memo.len()
    }

    fn drop_key(&mut self, key: NodeMemoKey) {
        if self.memo.remove(&key).is_none() {
            return;
        }
        self.evictions += 1;
        let epoch = key.2;
        if epoch == 0 {
            return;
        }
        if let Some(epoch_keys) = self.epoch_keys.get_mut(&epoch) {
            epoch_keys.remove(&key);
            if epoch_keys.is_empty() {
                self.epoch_keys.remove(&epoch);
            }
        }
    }

    fn maybe_autodisable(&mut self) {
        if !self.enabled {
            return;
        }
        if self.misses < self.miss_threshold {
            return;
        }
        // Autodisable when hit-ratio is too low: hits * denom <= misses.
        if self.hits * self.hit_ratio_denom > self.misses {
            return;
        }
        self.evictions += self.memo.len();
        self.memo.clear();
        self.epoch_keys.clear();
        self.prune_tick = 0;
        self.enabled = false;
    }
}

// ── CacheRuntimeOps ───────────────────────────────────────────────────────────

/// Facade combining node-level memo and skip-whitespace caching.
pub struct CacheRuntimeOps {
    pub node_memo: NodeMemoOps,
    pub skip_memo: BoundedMap<usize, usize>,
}

impl Default for CacheRuntimeOps {
    fn default() -> Self {
        Self::new(
            NODE_MEMO_AUTODISABLE_MISS_THRESHOLD,
            NODE_MEMO_AUTODISABLE_HIT_RATIO_DENOM,
            None,
            None,
        )
    }
}

impl CacheRuntimeOps {
    pub fn new(
        node_memo_miss_threshold: usize,
        node_memo_hit_ratio_denom: usize,
        node_memo_window: Option<usize>,
        node_memo_limit: Option<usize>,
    ) -> Self {
        Self {
            node_memo: NodeMemoOps::new(
                node_memo_miss_threshold,
                node_memo_hit_ratio_denom,
                node_memo_window,
                node_memo_limit,
                None,
            ),
            skip_memo: BoundedMap::with_capacity(SKIP_MEMO_LIMIT),
        }
    }

    /// Cache a skip-whitespace result: `pos → end_pos`.
    pub fn cache_skip(&mut self, pos: usize, end_pos: usize) {
        self.skip_memo.insert(pos, end_pos);
    }

    /// Look up a cached skip-whitespace result.
    pub fn lookup_skip(&self, pos: usize) -> Option<usize> {
        self.skip_memo.get(&pos).copied()
    }
}

// ── Inline element memo helpers ───────────────────────────────────────────────

/// Node-id prefix used as a pseudo-rule name in the rule memo.
fn rep_key(node_id: u32) -> String {
    format!("__rep_{}__", node_id)
}

/// Look up a repetition element result from the rule memo table.
pub fn inline_elem_memo_get(
    rule_memo: &HashMap<RuleMemoKey, MemoEntry>,
    node_id: u32,
    pos: usize,
) -> Option<&MemoEntry> {
    rule_memo.get(&(rep_key(node_id), pos))
}

/// Store a repetition element result in the rule memo table.
pub fn inline_elem_memo_put(
    rule_memo: &mut HashMap<RuleMemoKey, MemoEntry>,
    node_id: u32,
    pos: usize,
    entry: MemoEntry,
) {
    rule_memo.insert((rep_key(node_id), pos), entry);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ParseValue;

    // ── BoundedMap ────────────────────────────────────────────────────────────

    #[test]
    fn bounded_map_unbounded_insert_and_get() {
        let mut m: BoundedMap<i32, &str> = BoundedMap::unbounded();
        m.insert(1, "a");
        m.insert(2, "b");
        assert_eq!(m.get(&1), Some(&"a"));
        assert_eq!(m.get(&2), Some(&"b"));
        assert_eq!(m.len(), 2);
        assert_eq!(m.evictions, 0);
    }

    #[test]
    fn bounded_map_evicts_on_capacity_exceeded() {
        let mut m: BoundedMap<i32, i32> = BoundedMap::with_capacity(3);
        for i in 0..5 {
            m.insert(i, i * 10);
        }
        assert_eq!(m.len(), 3, "should stay at cap");
        assert_eq!(m.evictions, 2);
        // oldest (0, 1) evicted; 2, 3, 4 remain
        assert!(m.get(&0).is_none());
        assert!(m.get(&1).is_none());
        assert_eq!(m.get(&2), Some(&20));
    }

    #[test]
    fn bounded_map_update_does_not_grow_len() {
        let mut m: BoundedMap<&str, i32> = BoundedMap::with_capacity(2);
        m.insert("x", 1);
        m.insert("x", 2); // update in-place
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&"x"), Some(&2));
        assert_eq!(m.evictions, 0);
    }

    #[test]
    fn bounded_map_clear_resets_all() {
        let mut m: BoundedMap<i32, i32> = BoundedMap::with_capacity(10);
        m.insert(1, 1);
        m.clear();
        assert!(m.is_empty());
    }

    // ── OverlayMemo ───────────────────────────────────────────────────────────

    #[test]
    fn overlay_memo_reads_from_base() {
        let mut base = HashMap::new();
        base.insert("k1".to_string(), 42usize);
        let memo: OverlayMemo<String, usize> = OverlayMemo::new(Some(Arc::new(base)), None);
        assert_eq!(memo.get(&"k1".to_string()), Some(&42));
        assert_eq!(memo.get(&"missing".to_string()), None);
    }

    #[test]
    fn overlay_memo_overlay_shadows_base() {
        let mut base = HashMap::new();
        base.insert("k".to_string(), 1usize);
        let mut memo: OverlayMemo<String, usize> = OverlayMemo::new(Some(Arc::new(base)), None);
        memo.insert("k".to_string(), 99);
        assert_eq!(memo.get(&"k".to_string()), Some(&99));
    }

    #[test]
    fn overlay_memo_len_counts_distinct_keys() {
        let mut base = HashMap::new();
        base.insert("a".to_string(), 1usize);
        base.insert("b".to_string(), 2usize);
        let mut memo: OverlayMemo<String, usize> = OverlayMemo::new(Some(Arc::new(base)), None);
        // Shadow "a" in overlay, add new "c"
        memo.insert("a".to_string(), 10);
        memo.insert("c".to_string(), 30);
        // distinct: a, b, c → 3
        assert_eq!(memo.len(), 3);
    }

    #[test]
    fn overlay_memo_materialize_flattens() {
        let mut base = HashMap::new();
        base.insert("a".to_string(), 1usize);
        base.insert("b".to_string(), 2usize);
        let mut memo: OverlayMemo<String, usize> = OverlayMemo::new(Some(Arc::new(base)), None);
        memo.insert("b".to_string(), 99); // shadow b
        memo.insert("c".to_string(), 3);
        let flat = memo.materialize();
        assert_eq!(flat["a"], 1);
        assert_eq!(flat["b"], 99);
        assert_eq!(flat["c"], 3);
    }

    #[test]
    fn overlay_memo_bounded_evicts() {
        let memo_base: Option<Arc<HashMap<i32, i32>>> = None;
        let mut memo: OverlayMemo<i32, i32> = OverlayMemo::new(memo_base, Some(2));
        memo.insert(1, 10);
        memo.insert(2, 20);
        memo.insert(3, 30); // should evict 1
        assert_eq!(memo.evictions(), 1);
        assert_eq!(memo.get(&1), None);
        assert_eq!(memo.get(&3), Some(&30));
    }

    #[test]
    fn overlay_memo_clear_removes_base_and_overlay() {
        let mut base = HashMap::new();
        base.insert("x".to_string(), 5usize);
        let mut memo: OverlayMemo<String, usize> = OverlayMemo::new(Some(Arc::new(base)), None);
        memo.clear();
        assert!(memo.is_empty());
        assert_eq!(memo.get(&"x".to_string()), None);
    }

    // ── MemoEntry ─────────────────────────────────────────────────────────────

    #[test]
    fn memo_entry_success_and_failure() {
        let s = MemoEntry::success(10, ParseValue::Nil);
        assert!(s.ok);
        assert_eq!(s.pos, 10);
        assert!(s.value.is_some());

        let f = MemoEntry::failure(3);
        assert!(!f.ok);
        assert_eq!(f.pos, 3);
        assert!(f.value.is_none());
    }

    // ── RuleMemoState / NodeMemoState ─────────────────────────────────────────

    #[test]
    fn rule_memo_state_fields() {
        let s = RuleMemoState::new("root", 5, true);
        assert_eq!(s.key, ("root".to_string(), 5));
        assert!(s.use_memo);
    }

    #[test]
    fn node_memo_state_epoch_in_key() {
        let s = NodeMemoState::new(7, 12, 3);
        assert_eq!(s.key, (7, 12, 3));
        assert_eq!(s.epoch, 3);
    }

    // ── NodeMemoOps ───────────────────────────────────────────────────────────

    #[test]
    fn node_memo_ops_lookup_miss_and_store() {
        let mut ops = NodeMemoOps::default();
        let state = NodeMemoState::new(1, 0, 0);
        assert!(ops.lookup(&state).is_none());

        ops.store(&state, MemoEntry::success(5, ParseValue::Nil), 5);
        assert_eq!(ops.hits, 0);
        assert_eq!(ops.misses, 1);

        let hit = ops.lookup(&state);
        assert!(hit.is_some());
        assert_eq!(ops.hits, 1);
    }

    #[test]
    fn node_memo_ops_epoch_prune() {
        let mut ops = NodeMemoOps::default();
        let s1 = NodeMemoState::new(1, 0, 1);
        let s2 = NodeMemoState::new(2, 4, 0);
        ops.store(&s1, MemoEntry::failure(0), 0);
        ops.store(&s2, MemoEntry::failure(4), 4);
        ops.prune_epoch(1);
        // s1 was in epoch 1, should be gone
        assert!(!ops.memo.contains_key(&s1.key));
        // s2 was in epoch 0, should survive
        assert!(ops.memo.contains_key(&s2.key));
        assert_eq!(ops.evictions, 1);
    }

    #[test]
    fn node_memo_ops_window_prune() {
        let mut ops = NodeMemoOps::new(256, 128, Some(10), None, Some(1));
        // Store entries at pos 0 and pos 5
        ops.store(&NodeMemoState::new(1, 0, 0), MemoEntry::failure(0), 0);
        ops.store(&NodeMemoState::new(2, 5, 0), MemoEntry::failure(5), 5);
        // Prune with current_pos=20: window=10, cutoff=10, so pos 0 and 5 are stale
        ops.prune_budget(20, true);
        assert_eq!(ops.memo_len(), 0);
        assert_eq!(ops.evictions, 2);
    }

    #[test]
    fn node_memo_ops_limit_eviction() {
        let mut ops = NodeMemoOps::new(256, 128, None, Some(2), Some(1));
        for i in 0u32..4 {
            ops.store(
                &NodeMemoState::new(i, i as usize, 0),
                MemoEntry::failure(0),
                0,
            );
        }
        ops.prune_budget(0, true);
        assert!(ops.memo_len() <= 2);
    }

    #[test]
    fn node_memo_ops_autodisable_fires() {
        // Low threshold: disable after 3 misses with 0 hits
        let mut ops = NodeMemoOps::new(3, 1, None, None, Some(1));
        for i in 0u32..3 {
            ops.store(&NodeMemoState::new(i, 0, 0), MemoEntry::failure(0), 0);
        }
        assert!(!ops.is_enabled(), "should have autodisabled");
        assert_eq!(ops.memo_len(), 0);
    }

    #[test]
    fn node_memo_ops_autodisable_suppressed_by_hits() {
        // Autodisable condition: `hits * denom <= misses`.
        // With denom=2 and miss_threshold=3: needs hits*2 > misses to stay enabled.
        // Strategy: store 2 entries (misses=2, below threshold, no check yet),
        // collect 4 hits, then store 4 more entries; at each store check:
        //   misses=3: 4*2=8 > 3 → ok
        //   misses=4: 8 > 4 → ok
        //   ...stays enabled throughout.
        let mut ops = NodeMemoOps::new(3, 2, None, None, Some(1));
        // 2 stores — below threshold, no autodisable check fires yet
        ops.store(
            &NodeMemoState::new(0, 0, 0),
            MemoEntry::success(1, ParseValue::Nil),
            0,
        );
        ops.store(
            &NodeMemoState::new(1, 1, 0),
            MemoEntry::success(2, ParseValue::Nil),
            0,
        );
        // Collect 4 hits so ratio stays good
        ops.lookup(&NodeMemoState::new(0, 0, 0));
        ops.lookup(&NodeMemoState::new(0, 0, 0));
        ops.lookup(&NodeMemoState::new(1, 1, 0));
        ops.lookup(&NodeMemoState::new(1, 1, 0));
        // Now push past threshold — each store checks hits*2 > misses
        ops.store(&NodeMemoState::new(2, 2, 0), MemoEntry::failure(2), 2);
        ops.store(&NodeMemoState::new(3, 3, 0), MemoEntry::failure(3), 3);
        assert!(ops.is_enabled(), "should stay enabled with high hit ratio");
    }

    // ── CacheRuntimeOps ───────────────────────────────────────────────────────

    #[test]
    fn cache_runtime_ops_skip_memo() {
        let mut ops = CacheRuntimeOps::default();
        assert_eq!(ops.lookup_skip(5), None);
        ops.cache_skip(5, 8);
        assert_eq!(ops.lookup_skip(5), Some(8));
    }

    #[test]
    fn cache_runtime_ops_skip_memo_bounded() {
        let mut ops = CacheRuntimeOps::default();
        // Skip memo has SKIP_MEMO_LIMIT capacity; insert more should evict
        for i in 0..(SKIP_MEMO_LIMIT + 10) {
            ops.cache_skip(i, i + 1);
        }
        assert!(ops.skip_memo.evictions > 0);
        assert_eq!(ops.skip_memo.len(), SKIP_MEMO_LIMIT);
    }

    // ── Inline elem memo helpers ──────────────────────────────────────────────

    #[test]
    fn inline_elem_memo_roundtrip() {
        let mut rule_memo: HashMap<RuleMemoKey, MemoEntry> = HashMap::new();
        let entry = MemoEntry::success(4, ParseValue::Text("hi".into()));
        inline_elem_memo_put(&mut rule_memo, 42, 0, entry.clone());
        let got = inline_elem_memo_get(&rule_memo, 42, 0);
        assert_eq!(got, Some(&entry));
        assert_eq!(inline_elem_memo_get(&rule_memo, 42, 1), None);
    }
}
