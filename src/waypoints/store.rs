//! Authoritative in-memory shared-waypoint state.
//!
//! One global revision numbers the whole catalog. Every accepted mutation
//! advances it by exactly one, and the point it wrote carries the new revision -
//! which is what lets the client apply a delta and tell a lost one apart from an
//! out-of-order arrival. Two rules follow from that and are enforced here:
//!
//! * a mutation is *prepared* against the current state and only *committed*
//!   afterwards, so a durable write that fails cannot leave a revision the
//!   client has been told about; and
//! * nothing is committed against a revision other than the one it was prepared
//!   on, so a reload or a second writer cannot silently interleave.

use std::collections::HashMap;

use crate::identity::Id;
use crate::waypoints::model::LocationKey;
use crate::waypoints::proto::Waypoint;

/// An immutable view of the whole catalog at one revision.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The catalog's revision.
    pub revision: i64,
    /// Every point, in insertion order.
    pub waypoints: Vec<Waypoint>,
}

impl Snapshot {
    /// An empty catalog at revision zero.
    pub fn empty() -> Self {
        Snapshot {
            revision: 0,
            waypoints: Vec::new(),
        }
    }
}

/// What a delta does to a subscriber's copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaKind {
    /// One point was added or replaced.
    Upsert,
    /// One point was removed.
    Remove,
    /// Nothing observable changed.
    Noop,
}

/// One state change, shaped for the wire.
#[derive(Clone, Debug, PartialEq)]
pub struct Delta {
    /// Which field carries the payload.
    pub kind: DeltaKind,
    /// The revision the change produced.
    pub revision: i64,
    /// The point, for [`DeltaKind::Upsert`].
    pub waypoint: Option<Waypoint>,
    /// The removed id, for [`DeltaKind::Remove`].
    pub removed_id: Option<Id>,
}

impl Delta {
    /// A point was added or replaced.
    pub fn upsert(waypoint: Waypoint, revision: i64) -> Self {
        Delta {
            kind: DeltaKind::Upsert,
            revision,
            waypoint: Some(waypoint),
            removed_id: None,
        }
    }

    /// A point was removed.
    pub fn remove(id: Id, revision: i64) -> Self {
        Delta {
            kind: DeltaKind::Remove,
            revision,
            waypoint: None,
            removed_id: Some(id),
        }
    }

    /// Nothing changed.
    pub fn noop(revision: i64) -> Self {
        Delta {
            kind: DeltaKind::Noop,
            revision,
            waypoint: None,
            removed_id: None,
        }
    }
}

/// Why a mutation could not be prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The waypoint's id is already in the catalog.
    DuplicateId,
    /// Another point already occupies that block.
    DuplicateLocation,
    /// No such waypoint.
    Missing,
    /// The catalog's revision cannot be advanced.
    RevisionExhausted,
    /// A prepared mutation was committed against a different revision.
    Stale,
    /// A loaded snapshot contradicts itself.
    Malformed,
}

/// A mutation prepared against a revision but not yet committed.
#[derive(Clone, Debug)]
pub struct Prepared {
    base_revision: i64,
    snapshot: Snapshot,
    delta: Delta,
}

impl Prepared {
    /// The state the mutation would produce.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The wire-shaped change the mutation would make.
    pub fn delta(&self) -> &Delta {
        &self.delta
    }
}

/// The catalog.
#[derive(Clone, Debug)]
pub struct Store {
    revision: i64,
    waypoints: Vec<Waypoint>,
    index: HashMap<Id, usize>,
    occupancy: HashMap<LocationKey, Id>,
}

impl Store {
    /// Builds a store over a loaded snapshot.
    ///
    /// A snapshot that contradicts itself - a point whose revision is outside
    /// the catalog's, or a duplicate id - is rejected here rather than served.
    /// Duplicate *locations* are tolerated: a file written by an older build may
    /// contain them, and the reference implementation keeps such a file readable
    /// while still refusing any new point for the occupied block.
    pub fn new(initial: Snapshot) -> Result<Self, StoreError> {
        if initial.revision < 0 {
            return Err(StoreError::Malformed);
        }
        let mut store = Store {
            revision: initial.revision,
            waypoints: Vec::with_capacity(initial.waypoints.len()),
            index: HashMap::with_capacity(initial.waypoints.len()),
            occupancy: HashMap::with_capacity(initial.waypoints.len()),
        };
        for waypoint in initial.waypoints {
            if waypoint.revision < 1 || waypoint.revision > initial.revision {
                return Err(StoreError::Malformed);
            }
            if store.index.contains_key(&waypoint.id) {
                return Err(StoreError::Malformed);
            }
            store
                .occupancy
                .entry(LocationKey::from(&waypoint))
                .or_insert(waypoint.id);
            store.index.insert(waypoint.id, store.waypoints.len());
            store.waypoints.push(waypoint);
        }
        Ok(store)
    }

    /// An empty catalog.
    pub fn empty() -> Self {
        Store {
            revision: 0,
            waypoints: Vec::new(),
            index: HashMap::new(),
            occupancy: HashMap::new(),
        }
    }

    /// The current revision.
    pub fn revision(&self) -> i64 {
        self.revision
    }

    /// How many points are stored.
    pub fn len(&self) -> usize {
        self.waypoints.len()
    }

    /// A copy of the whole catalog.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            waypoints: self.waypoints.clone(),
        }
    }

    /// The point with this id, if it exists.
    pub fn find(&self, id: Id) -> Option<&Waypoint> {
        self.index
            .get(&id)
            .and_then(|position| self.waypoints.get(*position))
    }

    /// The point occupying this block, if one does.
    pub fn find_at(&self, location: &LocationKey) -> Option<&Waypoint> {
        let id = self.occupancy.get(location)?;
        self.waypoints
            .iter()
            .find(|waypoint| waypoint.id == *id)
    }

    /// How many points this player has published.
    pub fn count_published_by(&self, publisher: Id) -> usize {
        self.waypoints
            .iter()
            .filter(|waypoint| waypoint.publisher_id == publisher)
            .count()
    }

    /// Prepares an addition.
    pub fn prepare_create(&self, waypoint: Waypoint) -> Result<Prepared, StoreError> {
        if self.index.contains_key(&waypoint.id) {
            return Err(StoreError::DuplicateId);
        }
        let location = LocationKey::from(&waypoint);
        if self.occupancy.contains_key(&location) {
            return Err(StoreError::DuplicateLocation);
        }
        let next = self.next_revision()?;
        if waypoint.revision != next {
            return Err(StoreError::Malformed);
        }
        let mut waypoints = self.waypoints.clone();
        waypoints.push(waypoint.clone());
        Ok(Prepared {
            base_revision: self.revision,
            snapshot: Snapshot {
                revision: next,
                waypoints,
            },
            delta: Delta::upsert(waypoint, next),
        })
    }

    /// Prepares a removal.
    pub fn prepare_delete(&self, id: Id) -> Result<Prepared, StoreError> {
        let position = *self.index.get(&id).ok_or(StoreError::Missing)?;
        let next = self.next_revision()?;
        let mut waypoints = self.waypoints.clone();
        waypoints.remove(position);
        Ok(Prepared {
            base_revision: self.revision,
            snapshot: Snapshot {
                revision: next,
                waypoints,
            },
            delta: Delta::remove(id, next),
        })
    }

    /// Prepares a replacement.
    pub fn prepare_update(&self, waypoint: Waypoint) -> Result<Prepared, StoreError> {
        let position = *self
            .index
            .get(&waypoint.id)
            .ok_or(StoreError::Missing)?;
        let next = self.next_revision()?;
        if waypoint.revision != next {
            return Err(StoreError::Malformed);
        }
        let current = &self.waypoints[position];
        let updated_location = LocationKey::from(&waypoint);
        if updated_location != LocationKey::from(current) {
            match self.occupancy.get(&updated_location) {
                // Moving onto a block somebody else holds is a conflict; moving
                // nowhere, or onto a block this same point already holds, is not.
                Some(occupant) if *occupant != waypoint.id => {
                    return Err(StoreError::DuplicateLocation);
                }
                _ => {}
            }
        }
        let mut waypoints = self.waypoints.clone();
        waypoints[position] = waypoint.clone();
        Ok(Prepared {
            base_revision: self.revision,
            snapshot: Snapshot {
                revision: next,
                waypoints,
            },
            delta: Delta::upsert(waypoint, next),
        })
    }

    /// Installs a prepared mutation, provided nothing moved since it was made.
    pub fn commit(&mut self, prepared: Prepared) -> Result<(), StoreError> {
        if prepared.base_revision != self.revision {
            return Err(StoreError::Stale);
        }
        self.revision = prepared.snapshot.revision;
        self.waypoints.clear();
        self.index.clear();
        self.occupancy.clear();
        for waypoint in prepared.snapshot.waypoints {
            self.occupancy
                .entry(LocationKey::from(&waypoint))
                .or_insert(waypoint.id);
            let position = self.waypoints.len();
            self.index.insert(waypoint.id, position);
            self.waypoints.push(waypoint);
        }
        Ok(())
    }

    fn next_revision(&self) -> Result<i64, StoreError> {
        self.revision
            .checked_add(1)
            .ok_or(StoreError::RevisionExhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waypoints::proto::WaypointKind;

    fn waypoint(id: u64, revision: i64, x: f64) -> Waypoint {
        Waypoint {
            id: Id { high: 0, low: id },
            publisher_id: Id {
                high: 0,
                low: 0xaaaa,
            },
            publisher_name: "Steve".to_string(),
            name: format!("point {id}"),
            dimension_id: "minecraft:overworld".to_string(),
            x,
            y: 64.0,
            z: 0.0,
            color_argb: 0xff00_ff00u32 as i32,
            kind: WaypointKind::Normal,
            icon_item_id: String::new(),
            marker_label: String::new(),
            created_at_ms: 1_000,
            revision,
        }
    }

    #[test]
    fn an_empty_catalog_is_at_revision_zero() {
        let store = Store::empty();
        assert_eq!(store.revision(), 0);
        assert_eq!(store.len(), 0);
        assert_eq!(store.snapshot(), Snapshot::empty());
    }

    #[test]
    fn a_create_advances_the_revision_and_records_the_change() {
        let mut store = Store::empty();
        let prepared = store
            .prepare_create(waypoint(1, 1, 0.0))
            .expect("first point fits");
        assert_eq!(prepared.snapshot().revision, 1);
        assert_eq!(prepared.delta().kind, DeltaKind::Upsert);
        assert_eq!(
            prepared.delta().waypoint.as_ref().map(|w| w.id.low),
            Some(1)
        );
        // Nothing is visible until the commit.
        assert_eq!(store.len(), 0);
        store.commit(prepared).expect("commit on the same revision");
        assert_eq!(store.revision(), 1);
        assert_eq!(store.len(), 1);
        assert!(store.find(Id { high: 0, low: 1 }).is_some());
    }

    #[test]
    fn a_point_must_carry_the_revision_it_creates() {
        let store = Store::empty();
        assert_eq!(
            store.prepare_create(waypoint(1, 7, 0.0)).err(),
            Some(StoreError::Malformed),
            "a create recorded at the wrong revision would confuse every client"
        );
        assert!(store.prepare_create(waypoint(1, 1, 0.0)).is_ok());
    }

    #[test]
    fn one_block_holds_one_point() {
        let mut store = Store::empty();
        store
            .commit(store.prepare_create(waypoint(1, 1, 12.5)).expect("fits"))
            .expect("commit");
        assert_eq!(
            store.prepare_create(waypoint(2, 2, 12.9)).err(),
            Some(StoreError::DuplicateLocation),
            "12.9 floors into the same block as 12.5"
        );
        assert!(store.prepare_create(waypoint(2, 2, 13.0)).is_ok());
    }

    #[test]
    fn a_point_keeps_its_block_when_it_is_replaced_at_the_same_spot() {
        let mut store = Store::empty();
        store
            .commit(store.prepare_create(waypoint(1, 1, 12.5)).expect("fits"))
            .expect("commit");
        let mut moved = waypoint(1, 2, 12.0);
        moved.name = "renamed".to_string();
        store
            .commit(store.prepare_update(moved).expect("same block, own point"))
            .expect("commit");
        assert_eq!(store.revision(), 2);
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.find(Id { high: 0, low: 1 }).map(|w| w.name.as_str()),
            Some("renamed")
        );
    }

    #[test]
    fn a_point_cannot_be_moved_onto_another_point() {
        let mut store = Store::empty();
        store
            .commit(store.prepare_create(waypoint(1, 1, 12.0)).expect("fits"))
            .expect("commit");
        store
            .commit(store.prepare_create(waypoint(2, 2, 40.0)).expect("fits"))
            .expect("commit");
        let moved = waypoint(2, 3, 12.4);
        assert_eq!(
            store.prepare_update(moved).err(),
            Some(StoreError::DuplicateLocation)
        );
    }

    #[test]
    fn a_delete_removes_the_point_and_frees_its_block() {
        let mut store = Store::empty();
        store
            .commit(store.prepare_create(waypoint(1, 1, 12.5)).expect("fits"))
            .expect("commit");
        let prepared = store.prepare_delete(Id { high: 0, low: 1 }).expect("exists");
        assert_eq!(prepared.delta().kind, DeltaKind::Remove);
        assert_eq!(
            prepared.delta().removed_id.map(|id| id.low),
            Some(1)
        );
        store.commit(prepared).expect("commit");
        assert_eq!(store.revision(), 2);
        assert_eq!(store.len(), 0);
        // The block is free again, and the new point takes revision 3.
        assert!(store.prepare_create(waypoint(2, 3, 12.5)).is_ok());
    }

    #[test]
    fn a_prepared_mutation_cannot_be_committed_twice() {
        let mut store = Store::empty();
        let prepared = store.prepare_create(waypoint(1, 1, 0.0)).expect("fits");
        store.commit(prepared.clone()).expect("first commit");
        assert_eq!(
            store.commit(prepared).err(),
            Some(StoreError::Stale),
            "the second commit would overwrite revision 2 with revision 1"
        );
    }

    #[test]
    fn deleting_something_that_is_not_there_is_not_a_change() {
        let store = Store::empty();
        assert_eq!(
            store.prepare_delete(Id { high: 0, low: 9 }).err(),
            Some(StoreError::Missing)
        );
        assert_eq!(
            store.prepare_update(waypoint(9, 0, 0.0)).err(),
            Some(StoreError::Missing)
        );
    }

    #[test]
    fn a_loaded_catalog_is_indexed_and_checked() {
        let snapshot = Snapshot {
            revision: 5,
            waypoints: vec![waypoint(1, 5, 12.0), waypoint(2, 3, 40.0)],
        };
        let store = Store::new(snapshot).expect("consistent snapshot");
        assert_eq!(store.revision(), 5);
        assert_eq!(store.len(), 2);
        assert_eq!(store.count_published_by(Id { high: 0, low: 0xaaaa }), 2);
        assert!(store.find_at(&LocationKey::of("minecraft:overworld", 12.9, 64.0, 0.0).expect("finite")).is_some());
    }

    #[test]
    fn an_impossible_loaded_catalog_is_rejected() {
        // A point from the future.
        assert_eq!(
            Store::new(Snapshot {
                revision: 1,
                waypoints: vec![waypoint(1, 2, 0.0)],
            })
            .err(),
            Some(StoreError::Malformed)
        );
        // A point at revision zero.
        assert_eq!(
            Store::new(Snapshot {
                revision: 0,
                waypoints: vec![waypoint(1, 0, 0.0)],
            })
            .err(),
            Some(StoreError::Malformed)
        );
        // The same id twice.
        assert_eq!(
            Store::new(Snapshot {
                revision: 2,
                waypoints: vec![waypoint(1, 1, 0.0), waypoint(1, 2, 0.0)],
            })
            .err(),
            Some(StoreError::Malformed)
        );
    }

    #[test]
    fn a_duplicate_block_from_an_older_file_stays_readable() {
        let store = Store::new(Snapshot {
            revision: 2,
            waypoints: vec![waypoint(1, 1, 5.0), waypoint(2, 2, 5.5)],
        })
        .expect("historical duplicates are tolerated");
        assert_eq!(store.len(), 2, "both points are still served");
        assert_eq!(
            store.prepare_create(waypoint(3, 3, 5.9)).err(),
            Some(StoreError::DuplicateLocation),
            "but the block is already taken"
        );
    }
}
