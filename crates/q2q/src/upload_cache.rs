//! Client-wide bounded LRU and per-publication single-flight ownership.
use crate::upload_source::SourceRevision;
use q2q_core::merkle::Proof;
use std::{
  collections::HashMap,
  sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub(crate) const DEFAULT_CACHE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_CACHE_BYTES: usize = 256 * 1024 * 1024;

pub(crate) struct VerifiedBlock {
  pub(crate) bytes: Box<[u8]>,
  pub(crate) proof: Proof,
  pub(crate) revision: Option<SourceRevision>,
}
impl VerifiedBlock {
  pub(crate) fn new(
    bytes: Vec<u8>,
    mut proof: Proof,
    revision: Option<SourceRevision>,
  ) -> Arc<Self> {
    proof.leaf_hashes.shrink_to_fit();
    proof.nodes.shrink_to_fit();
    Arc::new(Self {
      bytes: bytes.into_boxed_slice(),
      proof,
      revision,
    })
  }
  fn cost(&self) -> usize {
    self.bytes.len()
      + self.proof.leaf_hashes.capacity() * size_of::<q2q_core::merkle::Hash>()
      + self.proof.nodes.capacity() * size_of::<q2q_core::merkle::Node>()
      + size_of::<Self>()
      + size_of::<Key>()
      + size_of::<Entry>()
  }
}
type Key = (u64, usize);
struct Entry {
  block: Arc<VerifiedBlock>,
  touched: u128,
  cost: usize,
}
#[derive(Default)]
struct Budget {
  entries: HashMap<Key, Entry>,
  used: usize,
  clock: u128,
  source: u64,
}
pub(crate) struct UploadCache {
  capacity: usize,
  budget: Mutex<Budget>,
}
impl UploadCache {
  pub(crate) fn new(capacity: usize) -> Arc<Self> {
    Arc::new(Self {
      capacity,
      budget: Mutex::new(Budget::default()),
    })
  }
  #[cfg(test)]
  pub(crate) fn capacity(&self) -> usize {
    self.capacity
  }

  pub(crate) fn source(self: &Arc<Self>) -> Arc<SourceCache> {
    let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
    budget.source = budget
      .source
      .checked_add(1)
      .expect("publication identity exhausted");
    Arc::new(SourceCache {
      pool: self.clone(),
      id: budget.source,
      flights: Mutex::new(Flights::default()),
    })
  }
  fn get(&self, key: Key) -> Option<Arc<VerifiedBlock>> {
    let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
    budget.clock += 1;
    let clock = budget.clock;
    let entry = budget.entries.get_mut(&key)?;
    entry.touched = clock;
    Some(entry.block.clone())
  }
  fn insert(&self, key: Key, block: Arc<VerifiedBlock>) {
    let cost = block.cost();
    let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = budget.entries.remove(&key) {
      budget.used -= old.cost;
    }
    if cost > self.capacity {
      return;
    }
    while cost > self.capacity - budget.used {
      let key = *budget
        .entries
        .iter()
        .min_by_key(|(_, entry)| entry.touched)
        .expect("nonempty cache over budget")
        .0;
      let old = budget.entries.remove(&key).expect("entry exists");
      budget.used -= old.cost;
    }
    budget.clock += 1;
    let touched = budget.clock;
    budget.entries.insert(
      key,
      Entry {
        block,
        touched,
        cost,
      },
    );
    budget.used += cost;
  }
  fn remove(&self, key: Key, expected: &Arc<VerifiedBlock>) {
    let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
    if budget
      .entries
      .get(&key)
      .is_some_and(|entry| Arc::ptr_eq(&entry.block, expected))
    {
      let old = budget.entries.remove(&key).expect("entry exists");
      budget.used -= old.cost;
    }
  }
  fn clear(&self, source: u64) {
    let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
    let mut released = 0;
    budget.entries.retain(|(id, _), entry| {
      if *id == source {
        released += entry.cost;
        false
      } else {
        true
      }
    });
    budget.used -= released;
  }
}

#[derive(Clone)]
pub(crate) enum FlightState {
  Loading,
  Ready(Arc<VerifiedBlock>),
  Retry,
  Closed,
}
#[derive(Default)]
struct Flights {
  closed: bool,
  active: HashMap<usize, Arc<watch::Sender<FlightState>>>,
}
pub(crate) struct SourceCache {
  pool: Arc<UploadCache>,
  id: u64,
  flights: Mutex<Flights>,
}
pub(crate) enum Lookup {
  Hit(Arc<VerifiedBlock>),
  Wait(watch::Receiver<FlightState>),
  Load(FlightGuard),
  Closed,
}
impl SourceCache {
  pub(crate) fn lookup(self: &Arc<Self>, index: usize) -> Lookup {
    // Namespace -> global budget is the only lock order. No I/O or async wait
    // holds either lock; LRU eviction never locks another namespace.
    let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
    if flights.closed {
      return Lookup::Closed;
    }
    if let Some(block) = self.pool.get((self.id, index)) {
      return Lookup::Hit(block);
    }
    if let Some(flight) = flights.active.get(&index) {
      return Lookup::Wait(flight.subscribe());
    }
    let flight = Arc::new(watch::channel(FlightState::Loading).0);
    flights.active.insert(index, flight.clone());
    Lookup::Load(FlightGuard {
      source: self.clone(),
      index,
      flight,
      active: true,
    })
  }
  pub(crate) fn invalidate(&self, index: usize, block: &Arc<VerifiedBlock>) {
    self.pool.remove((self.id, index), block);
  }
  pub(crate) fn is_closed(&self) -> bool {
    self
      .flights
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .closed
  }
  pub(crate) fn close(&self) {
    let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
    flights.closed = true;
    self.pool.clear(self.id);
    for (_, flight) in flights.active.drain() {
      flight.send_replace(FlightState::Closed);
    }
  }
}
impl Drop for SourceCache {
  fn drop(&mut self) {
    self.close();
  }
}

pub(crate) struct FlightGuard {
  source: Arc<SourceCache>,
  index: usize,
  flight: Arc<watch::Sender<FlightState>>,
  active: bool,
}
impl FlightGuard {
  pub(crate) fn complete(mut self, block: Arc<VerifiedBlock>, retain: bool) {
    self.finish(Some((block, retain)));
  }
  fn finish(&mut self, result: Option<(Arc<VerifiedBlock>, bool)>) {
    if !self.active {
      return;
    }
    self.active = false;
    let mut flights = self
      .source
      .flights
      .lock()
      .unwrap_or_else(|e| e.into_inner());
    if flights.closed {
      self.flight.send_replace(FlightState::Closed);
      return;
    }
    if !flights
      .active
      .get(&self.index)
      .is_some_and(|flight| Arc::ptr_eq(flight, &self.flight))
    {
      return;
    }
    flights.active.remove(&self.index);
    let state = match result {
      Some((block, retain)) => {
        if retain {
          self
            .source
            .pool
            .insert((self.source.id, self.index), block.clone());
        }
        FlightState::Ready(block)
      }
      None => FlightState::Retry,
    };
    self.flight.send_replace(state);
  }
}
impl Drop for FlightGuard {
  fn drop(&mut self) {
    self.finish(None);
  }
}

#[cfg(test)]
#[path = "../tests/upload_cache.rs"]
mod tests;
