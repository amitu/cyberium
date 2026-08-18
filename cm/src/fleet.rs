//! The controller's view of its workers: who is here, what they can do, who has
//! them, and when to take them back.
//!
//! All of it lives here because the controller is the only party that can see the
//! whole fleet. A worker knows only what it was told; a tester knows only what it
//! was granted. Neither can allocate, and neither should try.

use std::collections::{BTreeMap, BTreeSet};

use crate::proto::{Limits, Nivedana};

/// How long a reservation survives without being released, when the org has not
/// said otherwise. `policy.md` owns the real number: it has to sit above the longest
/// suite anyone runs here, which nothing in this file could know.
///
/// The backstop for a caller that dies mid-run. Releasing early is expected and
/// normal; this only stops a crashed tester stranding capacity forever.
pub const RESERVATION_SECS: u64 = 600;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct Worker {
    pub name: String,
    pub key: String,
    pub hints: Vec<String>,
    pub slots: u32,
    pub capabilities: Vec<String>,
    /// Reservations currently holding a slot here. Length is how busy it is.
    pub held_by: Vec<String>,
}

impl Worker {
    pub fn free_slots(&self) -> u32 {
        self.slots.saturating_sub(self.held_by.len() as u32)
    }

    /// Can this machine do everything the plea asked for?
    ///
    /// Every requested capability must be present. A machine that has extra is
    /// still a candidate — asking for `linux` should not exclude a machine that is
    /// also `gpu`, or the fleet fragments for no reason.
    pub fn satisfies(&self, wanted: &[String]) -> bool {
        wanted.iter().all(|w| self.capabilities.contains(w))
    }
}

#[derive(Debug, Clone)]
pub struct Reservation {
    pub id: String,
    /// The key that may use it. Bound into every ticket we minted for it.
    pub caller: String,
    pub alias: String,
    pub workers: Vec<String>,
    pub expires_at: u64,
}

#[derive(Debug, Default)]
pub struct Fleet {
    workers: BTreeMap<String, Worker>,
    reservations: BTreeMap<String, Reservation>,
    /// Monotonic, so reservation ids are unique within a controller's life.
    next: u64,
}

/// What allocating produced: the machines, and the reservation holding them.
#[derive(Debug)]
pub struct Allocation {
    pub reservation: String,
    pub workers: Vec<Worker>,
    pub limits: Limits,
    pub expires_in: u64,
}

/// A reservation nobody released, and what it was holding.
#[derive(Debug)]
pub struct Expired {
    pub id: String,
    /// Who had it. Worth carrying: "r7 expired" tells an operator less than
    /// "dana's r7 expired" when they are working out whose run died.
    pub alias: String,
    pub workers: Vec<String>,
}

/// Why allocation could not be satisfied in full.
#[derive(Debug)]
pub enum Shortfall {
    /// Nothing at all matches, and it never will without a fleet change.
    NoneCapable { wanted: Vec<String> },
    /// Machines exist but are busy, or there are fewer than asked for.
    Fewer { available: u32 },
}

impl Fleet {
    pub fn arrive(&mut self, worker: Worker) {
        self.workers.insert(worker.name.clone(), worker);
    }

    pub fn depart(&mut self, name: &str) {
        self.workers.remove(name);
        // Any reservation naming it keeps its other machines; the caller learns
        // when a dial fails, which is a clearer signal than silently shrinking a
        // grant they were already told about.
        for r in self.reservations.values_mut() {
            r.workers.retain(|w| w != name);
        }
    }

    // The operator's view, and the tests'. Not a caller's — nothing here reaches
    // anyone outside the organisation that runs this controller.
    pub fn workers(&self) -> impl Iterator<Item = &Worker> {
        self.workers.values()
    }

    pub fn reservations(&self) -> impl Iterator<Item = &Reservation> {
        self.reservations.values()
    }

    /// What is here — for the **operator**, not for a caller.
    ///
    /// This used to answer a ping. It should not: a summary polled repeatedly tells
    /// another organisation your utilisation over time, and from that your release
    /// cadence and how often you have incidents. It lives behind controller
    /// introspection instead.
    pub fn summary(&self) -> (u32, u32, Vec<String>) {
        let machines = self.workers.len() as u32;
        let free = self.workers.values().filter(|w| w.free_slots() > 0).count() as u32;
        let capabilities: BTreeSet<String> = self
            .workers
            .values()
            .flat_map(|w| w.capabilities.iter().cloned())
            .collect();
        (machines, free, capabilities.into_iter().collect())
    }

    /// Machines that could serve this plea if they were free.
    pub fn capable(&self, wanted: &[String]) -> usize {
        self.workers.values().filter(|w| w.satisfies(wanted)).count()
    }

    /// Which machines *would* serve this, changing nothing.
    ///
    /// Split out from `allocate` so a dry run and a real one cannot drift. The
    /// tempting alternative — a second function that estimates — is a slow-motion
    /// bug: the day the two disagree, a dry run confidently reports a number the
    /// real path will not give. Allocating and immediately releasing is no better,
    /// since it briefly holds machines a concurrent caller then cannot see.
    pub fn choose(&self, wanted: &[String], count: u32) -> Result<Vec<String>, Shortfall> {
        if self.capable(wanted) == 0 {
            // Distinguished from "busy" on purpose: no amount of waiting fixes
            // this, and telling the caller to retry would be a lie.
            return Err(Shortfall::NoneCapable {
                wanted: wanted.to_vec(),
            });
        }

        let mut chosen: Vec<String> = Vec::new();
        for worker in self.workers.values() {
            if chosen.len() as u32 >= count {
                break;
            }
            if worker.satisfies(wanted) && worker.free_slots() > 0 {
                chosen.push(worker.name.clone());
            }
        }

        if (chosen.len() as u32) < count {
            return Err(Shortfall::Fewer {
                available: chosen.len() as u32,
            });
        }
        Ok(chosen)
    }

    /// Take `count` machines matching `nivedana`'s capabilities, and hold them.
    ///
    /// `lifetime` is passed in rather than held here: it comes from the *tenant's*
    /// policy, and with a policy per tenant there is no single number the fleet
    /// could own. The fleet tracks machines; how long a grant lasts is somebody
    /// else's rule.
    pub fn allocate(
        &mut self,
        nivedana: &Nivedana,
        count: u32,
        caller: &str,
        alias: &str,
        lifetime: u64,
    ) -> Result<Allocation, Shortfall> {
        let lifetime = lifetime.max(1);
        let chosen = self.choose(&nivedana.capabilities, count)?;

        self.next += 1;
        let id = format!("r{}", self.next);
        let expires_at = now() + lifetime;

        for name in &chosen {
            if let Some(w) = self.workers.get_mut(name) {
                w.held_by.push(id.clone());
            }
        }
        self.reservations.insert(
            id.clone(),
            Reservation {
                id: id.clone(),
                caller: caller.to_string(),
                alias: alias.to_string(),
                workers: chosen.clone(),
                expires_at,
            },
        );

        Ok(Allocation {
            workers: chosen
                .iter()
                .filter_map(|n| self.workers.get(n).cloned())
                .collect(),
            reservation: id,
            limits: Limits { max_seconds: lifetime },
            expires_in: lifetime,
        })
    }

    /// Give a reservation back. Returns the machines that were freed, so the
    /// controller can tell each of them.
    ///
    /// `caller` must be the key the reservation was granted to: a release is an
    /// instruction about someone else's capacity if anyone may send it.
    pub fn release(&mut self, id: &str, caller: &str) -> Result<Vec<String>, String> {
        let Some(reservation) = self.reservations.get(id) else {
            return Err(format!("no reservation {id}"));
        };
        if reservation.caller != caller {
            return Err(format!("{id} was not granted to you"));
        }
        Ok(self.free(id))
    }

    /// Free everything past its expiry. The backstop.
    /// Returns the reservation, **who held it**, and the machines taken back.
    ///
    /// The alias travels with it because the operator reading that log line wants to
    /// know whose run died, not only that a number expired.
    pub fn expire(&mut self) -> Vec<Expired> {
        let now = now();
        let stale: Vec<(String, String)> = self
            .reservations
            .values()
            .filter(|r| r.expires_at <= now)
            .map(|r| (r.id.clone(), r.alias.clone()))
            .collect();

        stale
            .into_iter()
            .map(|(id, alias)| {
                let workers = self.free(&id);
                Expired { id, alias, workers }
            })
            .collect()
    }

    fn free(&mut self, id: &str) -> Vec<String> {
        let Some(reservation) = self.reservations.remove(id) else {
            return Vec::new();
        };
        for name in &reservation.workers {
            if let Some(w) = self.workers.get_mut(name) {
                w.held_by.retain(|held| held != id);
            }
        }
        reservation.workers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(name: &str, slots: u32, caps: &[&str]) -> Worker {
        Worker {
            name: name.into(),
            key: format!("key-{name}"),
            hints: vec![],
            slots,
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
            held_by: vec![],
        }
    }

    fn plea(caps: &[&str]) -> Nivedana {
        Nivedana {
            why: "because".into(),
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
            ..Default::default()
        }
    }

    fn fleet() -> Fleet {
        let mut f = Fleet::default();
        f.arrive(worker("linux-1", 1, &["linux"]));
        f.arrive(worker("linux-2", 1, &["linux"]));
        f.arrive(worker("gpu-1", 1, &["linux", "gpu"]));
        f
    }

    #[test]
    fn allocates_only_machines_that_can_do_the_work() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["gpu"]), 1, "caller", "dana", RESERVATION_SECS).unwrap();
        assert_eq!(a.workers.len(), 1);
        assert_eq!(a.workers[0].name, "gpu-1");
    }

    #[test]
    fn extra_capabilities_do_not_disqualify() {
        // Asking for linux must not exclude the machine that is also a gpu, or the
        // fleet fragments for no reason.
        let mut f = fleet();
        let a = f.allocate(&plea(&["linux"]), 3, "caller", "dana", RESERVATION_SECS).unwrap();
        assert_eq!(a.workers.len(), 3);
    }

    #[test]
    fn impossible_and_merely_busy_are_different_answers() {
        let mut f = fleet();
        match f.allocate(&plea(&["risc-v"]), 1, "caller", "dana", RESERVATION_SECS) {
            Err(Shortfall::NoneCapable { wanted }) => assert_eq!(wanted, vec!["risc-v".to_string()]),
            other => panic!("expected NoneCapable, got {other:?}"),
        }
        match f.allocate(&plea(&["gpu"]), 5, "caller", "dana", RESERVATION_SECS) {
            Err(Shortfall::Fewer { available }) => assert_eq!(available, 1),
            other => panic!("expected Fewer, got {other:?}"),
        }
    }

    #[test]
    fn an_allocated_machine_is_not_offered_twice() {
        let mut f = fleet();
        f.allocate(&plea(&["gpu"]), 1, "a", "dana", RESERVATION_SECS).unwrap();
        match f.allocate(&plea(&["gpu"]), 1, "b", "kiran", RESERVATION_SECS) {
            Err(Shortfall::Fewer { available }) => assert_eq!(available, 0),
            other => panic!("expected the machine to be busy, got {other:?}"),
        }
    }

    #[test]
    fn releasing_returns_the_machines() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["gpu"]), 1, "a", "dana", RESERVATION_SECS).unwrap();
        let freed = f.release(&a.reservation, "a").unwrap();
        assert_eq!(freed, vec!["gpu-1".to_string()]);
        // And it can be had again.
        assert!(f.allocate(&plea(&["gpu"]), 1, "b", "kiran", RESERVATION_SECS).is_ok());
    }

    #[test]
    fn only_the_holder_may_release() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["gpu"]), 1, "a", "dana", RESERVATION_SECS).unwrap();
        let err = f.release(&a.reservation, "someone-else").unwrap_err();
        assert!(err.contains("not granted to you"), "{err}");
        // And the machine is still held.
        assert_eq!(f.workers().find(|w| w.name == "gpu-1").unwrap().free_slots(), 0);
    }

    #[test]
    fn expiry_frees_a_reservation_nobody_released() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["gpu"]), 1, "a", "dana", RESERVATION_SECS).unwrap();
        assert!(f.expire().is_empty(), "not due yet");

        // Reach in and age it, rather than sleeping ten minutes.
        f.reservations.get_mut(&a.reservation).unwrap().expires_at = now() - 1;

        let expired = f.expire();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].workers, vec!["gpu-1".to_string()]);
        assert_eq!(expired[0].alias, "dana", "who lost it travels with it");
        assert!(f.allocate(&plea(&["gpu"]), 1, "b", "kiran", RESERVATION_SECS).is_ok());
    }

    #[test]
    fn a_departing_worker_leaves_its_reservations() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["linux"]), 2, "a", "dana", RESERVATION_SECS).unwrap();
        assert_eq!(a.workers.len(), 2);

        f.depart("linux-1");
        let held: Vec<_> = f.reservations().map(|r| r.workers.len()).collect();
        assert_eq!(held, vec![1], "the reservation keeps its surviving machine");
    }

    #[test]
    fn the_summary_answers_is_it_worth_asking() {
        let mut f = fleet();
        assert_eq!(
            f.summary(),
            (3, 3, vec!["gpu".to_string(), "linux".to_string()]),
            "capabilities are the union across the fleet, deduplicated"
        );

        f.allocate(&plea(&["gpu"]), 1, "a", "dana", RESERVATION_SECS).unwrap();
        let (machines, free, _) = f.summary();
        // Still three machines, but one of them is no longer on offer — which is
        // the difference between "ask later" and "never".
        assert_eq!((machines, free), (3, 2));

        assert_eq!(Fleet::default().summary(), (0, 0, vec![]));
    }

    #[test]
    fn choosing_takes_nothing() {
        // The property a dry run rests on. If choosing held anything, even
        // briefly, a concurrent caller would see a fleet busier than it is.
        let f = fleet();
        for _ in 0..3 {
            assert_eq!(f.choose(&["gpu".into()], 1).unwrap(), vec!["gpu-1".to_string()]);
        }
        assert_eq!(f.summary().1, 3, "still three free after three rehearsals");
    }

    #[test]
    fn a_rehearsal_and_a_real_ask_agree() {
        // Both go through `choose`, so they cannot drift — this pins that they are
        // wired that way rather than merely written to look alike.
        let mut f = fleet();
        let rehearsed = f.choose(&["linux".into()], 2).unwrap();
        let taken = f.allocate(&plea(&["linux"]), 2, "a", "dana", RESERVATION_SECS).unwrap();
        let names: Vec<String> = taken.workers.iter().map(|w| w.name.clone()).collect();
        assert_eq!(rehearsed, names);
    }

    #[test]
    fn a_rehearsal_sees_what_is_already_held() {
        let mut f = fleet();
        f.allocate(&plea(&["gpu"]), 1, "a", "dana", RESERVATION_SECS).unwrap();
        // Not "you could have one" — the machine is taken, and saying otherwise
        // would be the dry run lying about the present.
        match f.choose(&["gpu".into()], 1) {
            Err(Shortfall::Fewer { available }) => assert_eq!(available, 0),
            other => panic!("expected none free, got {other:?}"),
        }
    }

    #[test]
    fn the_lifetime_comes_from_outside() {
        // Per grant, because it comes from the tenant's policy and there is a policy
        // per tenant — the fleet could not own one number if it wanted to.
        let mut f = Fleet::default();
        f.arrive(worker("linux-1", 1, &["linux"]));
        let a = f.allocate(&plea(&["linux"]), 1, "a", "dana", 30).unwrap();
        assert_eq!(a.expires_in, 30);
        assert_eq!(a.limits.max_seconds, 30);

        // A zero would expire every grant the instant it was made.
        f.release(&a.reservation, "a").unwrap();
        let b = f.allocate(&plea(&["linux"]), 1, "a", "dana", 0).unwrap();
        assert_eq!(b.expires_in, 1);
    }

    #[test]
    fn slots_allow_more_than_one_reservation_per_machine() {
        let mut f = Fleet::default();
        f.arrive(worker("big", 3, &["linux"]));
        assert!(f.allocate(&plea(&["linux"]), 1, "a", "dana", RESERVATION_SECS).is_ok());
        assert!(f.allocate(&plea(&["linux"]), 1, "b", "kiran", RESERVATION_SECS).is_ok());
        assert_eq!(f.workers().next().unwrap().free_slots(), 1);
    }
}
