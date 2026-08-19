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
    pub capabilities: Vec<String>,
    /// Credits per minute while held. Announced by the machine, because the machine
    /// is the thing that knows what it is — the same reasoning as capabilities, and
    /// a second list kept centrally is a second list to get wrong.
    pub rate: u32,
    /// The reservation holding it, if any.
    ///
    /// **One at a time.** A worker used to advertise several concurrent slots, and
    /// that was the wrong place to put concurrency: run more `cm worker` processes
    /// instead, and let the operating system provide the limits and isolation it is
    /// already good at. It also removed two problems — what a rate means when two
    /// tenants share a machine, and the fact that a machine hosting two tenants at
    /// once has no moment *between* tenants to be cleaned in.
    pub held_by: Option<String>,
}

impl Worker {
    pub fn is_free(&self) -> bool {
        self.held_by.is_none()
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
    /// Who asked, for display.
    pub alias: String,
    /// Who **pays**. Not the same thing: several callers may share one tenant.
    pub tenant: String,
    pub workers: Vec<String>,
    /// Credits per minute for the whole grant, fixed when it was made.
    ///
    /// Recorded rather than looked up at close: a machine may have departed by then,
    /// and in any case you pay what was agreed when you took it.
    pub rate: u32,
    pub started_at: u64,
    pub expires_at: u64,
}

impl Reservation {
    /// What this has cost so far, in credits.
    ///
    /// Part-minutes count as a minute. Charging nothing for a fifty-second run would
    /// make a fleet free to anybody willing to churn.
    pub fn cost_by(&self, at: u64) -> u64 {
        let minutes = at.saturating_sub(self.started_at).div_ceil(60).max(1);
        minutes * self.rate as u64
    }

    /// The most this can still cost if nobody releases it — what a budget has to
    /// assume is already committed.
    pub fn worst_case(&self) -> u64 {
        self.cost_by(self.expires_at)
    }
}

#[derive(Debug, Default)]
pub struct Fleet {
    workers: BTreeMap<String, Worker>,
    reservations: BTreeMap<String, Reservation>,
    /// Monotonic, so reservation ids are unique within a controller's life.
    next: u64,
}

/// The fleet as a model sees it: how many could do this work, how many are free
/// right now, and what each free one costs per minute. No names, no holders.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Brief {
    pub fleet: u32,
    pub capable: u32,
    pub free: u32,
    /// Rates of the free capable machines, cheapest first — the order `choose`
    /// spends in, so a model adding them up gets the real price of N machines.
    pub rates: Vec<u32>,
}

/// What allocating produced: the machines, and the reservation holding them.
#[derive(Debug)]
pub struct Allocation {
    pub reservation: String,
    pub workers: Vec<Worker>,
    pub limits: Limits,
    pub expires_in: u64,
}

/// A reservation that has ended, released or expired, and what it cost.
#[derive(Debug)]
pub struct Closed {
    pub reservation: Reservation,
    pub minutes: u64,
    pub credits: u64,
}

impl Closed {
    pub fn id(&self) -> &str {
        &self.reservation.id
    }
    pub fn alias(&self) -> &str {
        &self.reservation.alias
    }
    pub fn tenant(&self) -> &str {
        &self.reservation.tenant
    }
    pub fn workers(&self) -> &[String] {
        &self.reservation.workers
    }
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
        let free = self.workers.values().filter(|w| w.is_free()).count() as u32;
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

        // **Cheapest first.** Asking for `linux` should not spend a GPU box's rate
        // when an ordinary one is idle — this is a cost-aware allocator, and picking
        // by name order made it accidentally indifferent to price. A stable sort, so
        // machines of equal rate still come in a predictable order.
        let mut candidates: Vec<&Worker> = self
            .workers
            .values()
            .filter(|w| w.satisfies(wanted) && w.is_free())
            .collect();
        candidates.sort_by_key(|w| w.rate);

        let chosen: Vec<String> = candidates
            .into_iter()
            .take(count as usize)
            .map(|w| w.name.clone())
            .collect();

        if (chosen.len() as u32) < count {
            return Err(Shortfall::Fewer {
                available: chosen.len() as u32,
            });
        }
        Ok(chosen)
    }

    /// What the model is told about the fleet: **counts and prices, never names**.
    ///
    /// It needs availability to answer "how many", since granting six machines when
    /// two are free is a promise the fleet cannot keep. It does not need to know
    /// which machines, or who is holding the rest — so it is not told, and cannot
    /// disclose what it was never given. Utilisation over time is still an operator's
    /// business only: this goes into one prompt for one decision, never into a Pong
    /// and never back to a caller.
    pub fn brief(&self, wanted: &[String]) -> Brief {
        let capable: Vec<&Worker> = self.workers.values().filter(|w| w.satisfies(wanted)).collect();
        let mut rates: Vec<u32> =
            capable.iter().filter(|w| w.is_free()).map(|w| w.rate).collect();
        // Ascending, because that is the order `choose` would actually spend in.
        rates.sort_unstable();
        Brief {
            fleet: self.workers.len() as u32,
            capable: capable.len() as u32,
            free: rates.len() as u32,
            rates,
        }
    }

    /// Credits per minute for a set of machines.
    pub fn rate_of(&self, names: &[String]) -> u32 {
        names
            .iter()
            .filter_map(|n| self.workers.get(n))
            .map(|w| w.rate)
            .sum()
    }

    /// The most this tenant's open reservations can still cost.
    ///
    /// A budget that looked only at what has been *spent* would let somebody start a
    /// hundred runs at once while comfortably under it, and find out afterwards.
    pub fn committed(&self, tenant: &str) -> u64 {
        self.reservations
            .values()
            .filter(|r| r.tenant == tenant)
            .map(|r| r.worst_case())
            .sum()
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
        tenant: &str,
        lifetime: u64,
    ) -> Result<Allocation, Shortfall> {
        let lifetime = lifetime.max(1);
        let chosen = self.choose(&nivedana.capabilities, count)?;
        let rate = self.rate_of(&chosen);

        self.next += 1;
        let id = format!("r{}", self.next);
        let expires_at = now() + lifetime;

        for name in &chosen {
            if let Some(w) = self.workers.get_mut(name) {
                w.held_by = Some(id.clone());
            }
        }
        self.reservations.insert(
            id.clone(),
            Reservation {
                id: id.clone(),
                caller: caller.to_string(),
                alias: alias.to_string(),
                tenant: tenant.to_string(),
                workers: chosen.clone(),
                rate,
                started_at: now(),
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
    pub fn release(&mut self, id: &str, caller: &str) -> Result<Closed, String> {
        let Some(reservation) = self.reservations.get(id) else {
            return Err(format!("no reservation {id}"));
        };
        if reservation.caller != caller {
            return Err(format!("{id} was not granted to you"));
        }
        Ok(self.close(id).expect("just found it"))
    }

    /// Free everything past its expiry. The backstop.
    /// Returns the reservation, **who held it**, and the machines taken back.
    ///
    /// The alias travels with it because the operator reading that log line wants to
    /// know whose run died, not only that a number expired.
    pub fn expire(&mut self) -> Vec<Closed> {
        let now = now();
        let stale: Vec<String> = self
            .reservations
            .values()
            .filter(|r| r.expires_at <= now)
            .map(|r| r.id.clone())
            .collect();

        stale.into_iter().filter_map(|id| self.close(&id)).collect()
    }

    /// Give the machines back, and say what it cost.
    ///
    /// One place, so a released reservation and an expired one are billed the same
    /// way — a caller who walks away pays for the time they held, which is exactly
    /// what they cost the fleet.
    fn close(&mut self, id: &str) -> Option<Closed> {
        let reservation = self.reservations.remove(id)?;
        for name in &reservation.workers {
            if let Some(w) = self.workers.get_mut(name) {
                w.held_by = None;
            }
        }
        let at = now();
        Some(Closed {
            credits: reservation.cost_by(at),
            minutes: at.saturating_sub(reservation.started_at).div_ceil(60).max(1),
            reservation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(name: &str, caps: &[&str]) -> Worker {
        priced(name, caps, 1)
    }

    fn priced(name: &str, caps: &[&str], rate: u32) -> Worker {
        Worker {
            name: name.into(),
            key: format!("key-{name}"),
            hints: vec![],
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
            rate,
            held_by: None,
        }
    }

    fn plea(caps: &[&str]) -> Nivedana {
        Nivedana {
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
            ..Default::default()
        }
    }

    fn fleet() -> Fleet {
        let mut f = Fleet::default();
        f.arrive(worker("linux-1", &["linux"]));
        f.arrive(worker("linux-2", &["linux"]));
        f.arrive(worker("gpu-1", &["linux", "gpu"]));
        f
    }

    #[test]
    fn allocates_only_machines_that_can_do_the_work() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["gpu"]), 1, "caller", "dana", "dana", RESERVATION_SECS).unwrap();
        assert_eq!(a.workers.len(), 1);
        assert_eq!(a.workers[0].name, "gpu-1");
    }

    #[test]
    fn extra_capabilities_do_not_disqualify() {
        // Asking for linux must not exclude the machine that is also a gpu, or the
        // fleet fragments for no reason.
        let mut f = fleet();
        let a = f.allocate(&plea(&["linux"]), 3, "caller", "dana", "dana", RESERVATION_SECS).unwrap();
        assert_eq!(a.workers.len(), 3);
    }

    #[test]
    fn impossible_and_merely_busy_are_different_answers() {
        let mut f = fleet();
        match f.allocate(&plea(&["risc-v"]), 1, "caller", "dana", "dana", RESERVATION_SECS) {
            Err(Shortfall::NoneCapable { wanted }) => assert_eq!(wanted, vec!["risc-v".to_string()]),
            other => panic!("expected NoneCapable, got {other:?}"),
        }
        match f.allocate(&plea(&["gpu"]), 5, "caller", "dana", "dana", RESERVATION_SECS) {
            Err(Shortfall::Fewer { available }) => assert_eq!(available, 1),
            other => panic!("expected Fewer, got {other:?}"),
        }
    }

    #[test]
    fn an_allocated_machine_is_not_offered_twice() {
        let mut f = fleet();
        f.allocate(&plea(&["gpu"]), 1, "a", "dana", "dana", RESERVATION_SECS).unwrap();
        match f.allocate(&plea(&["gpu"]), 1, "b", "kiran", "kiran", RESERVATION_SECS) {
            Err(Shortfall::Fewer { available }) => assert_eq!(available, 0),
            other => panic!("expected the machine to be busy, got {other:?}"),
        }
    }

    #[test]
    fn releasing_returns_the_machines() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["gpu"]), 1, "a", "dana", "dana", RESERVATION_SECS).unwrap();
        let closed = f.release(&a.reservation, "a").unwrap();
        assert_eq!(closed.workers(), ["gpu-1".to_string()]);
        // And it can be had again.
        assert!(f.allocate(&plea(&["gpu"]), 1, "b", "kiran", "kiran", RESERVATION_SECS).is_ok());
    }

    #[test]
    fn only_the_holder_may_release() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["gpu"]), 1, "a", "dana", "dana", RESERVATION_SECS).unwrap();
        let err = f.release(&a.reservation, "someone-else").unwrap_err();
        assert!(err.contains("not granted to you"), "{err}");
        // And the machine is still held.
        assert!(!f.workers().find(|w| w.name == "gpu-1").unwrap().is_free());
    }

    #[test]
    fn expiry_frees_a_reservation_nobody_released() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["gpu"]), 1, "a", "dana", "dana", RESERVATION_SECS).unwrap();
        assert!(f.expire().is_empty(), "not due yet");

        // Reach in and age it, rather than sleeping ten minutes.
        f.reservations.get_mut(&a.reservation).unwrap().expires_at = now() - 1;

        let expired = f.expire();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].workers(), ["gpu-1".to_string()]);
        assert_eq!(expired[0].alias(), "dana", "who lost it travels with it");
        assert!(f.allocate(&plea(&["gpu"]), 1, "b", "kiran", "kiran", RESERVATION_SECS).is_ok());
    }

    #[test]
    fn a_departing_worker_leaves_its_reservations() {
        let mut f = fleet();
        let a = f.allocate(&plea(&["linux"]), 2, "a", "dana", "dana", RESERVATION_SECS).unwrap();
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

        f.allocate(&plea(&["gpu"]), 1, "a", "dana", "dana", RESERVATION_SECS).unwrap();
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
        let taken = f.allocate(&plea(&["linux"]), 2, "a", "dana", "dana", RESERVATION_SECS).unwrap();
        let names: Vec<String> = taken.workers.iter().map(|w| w.name.clone()).collect();
        assert_eq!(rehearsed, names);
    }

    #[test]
    fn a_rehearsal_sees_what_is_already_held() {
        let mut f = fleet();
        f.allocate(&plea(&["gpu"]), 1, "a", "dana", "dana", RESERVATION_SECS).unwrap();
        // Not "you could have one" — the machine is taken, and saying otherwise
        // would be the dry run lying about the present.
        match f.choose(&["gpu".into()], 1) {
            Err(Shortfall::Fewer { available }) => assert_eq!(available, 0),
            other => panic!("expected none free, got {other:?}"),
        }
    }

    #[test]
    fn the_cheapest_capable_machine_goes_first() {
        // This is a *cost-aware* allocator, and picking in name order made it
        // accidentally indifferent to price: asking for linux could spend a GPU box's
        // rate while an ordinary one sat idle.
        let mut f = Fleet::default();
        f.arrive(priced("expensive", &["linux", "gpu"], 8));
        f.arrive(priced("cheap", &["linux"], 1));
        f.arrive(priced("middling", &["linux"], 3));

        assert_eq!(f.choose(&["linux".into()], 1).unwrap(), vec!["cheap".to_string()]);
        assert_eq!(
            f.choose(&["linux".into()], 2).unwrap(),
            vec!["cheap".to_string(), "middling".to_string()]
        );
        // A capability only the expensive machine has still gets it.
        assert_eq!(f.choose(&["gpu".into()], 1).unwrap(), vec!["expensive".to_string()]);
    }

    #[test]
    fn a_grant_is_priced_when_it_is_made() {
        let mut f = Fleet::default();
        f.arrive(priced("a", &["linux"], 3));
        f.arrive(priced("b", &["linux"], 5));
        let a = f.allocate(&plea(&["linux"]), 2, "k", "dana", "payments", 600).unwrap();

        let r = f.reservations().next().unwrap();
        assert_eq!(r.rate, 8, "the sum of both machines");
        assert_eq!(r.tenant, "payments", "who pays is not who asked");
        // A part-minute is a minute: a fifty-second run must not be free, or a fleet
        // is free to anybody willing to churn.
        assert_eq!(r.cost_by(r.started_at), 8);
        assert_eq!(r.cost_by(r.started_at + 61), 16);
        // And the worst case is the whole lifetime, which is what a budget must
        // assume is already committed.
        assert_eq!(r.worst_case(), 8 * 10);
        drop(a);
    }

    #[test]
    fn closing_reports_the_cost_and_releases_the_machines() {
        let mut f = Fleet::default();
        f.arrive(priced("a", &["linux"], 4));
        let a = f.allocate(&plea(&["linux"]), 1, "k", "dana", "payments", 600).unwrap();

        let closed = f.release(&a.reservation, "k").unwrap();
        assert_eq!(closed.credits, 4, "one minute minimum at 4/min");
        assert_eq!(closed.tenant(), "payments");
        assert!(f.workers().next().unwrap().is_free());
    }

    #[test]
    fn commitments_are_counted_before_they_are_spent() {
        // The hole a spend-only budget leaves: a hundred runs started at once while
        // comfortably under it, discovered afterwards.
        let mut f = Fleet::default();
        f.arrive(priced("a", &["linux"], 2));
        f.arrive(priced("b", &["linux"], 2));
        assert_eq!(f.committed("payments"), 0);

        f.allocate(&plea(&["linux"]), 2, "k", "dana", "payments", 600).unwrap();
        // Two machines at 2/min for 10 minutes, if nobody releases.
        assert_eq!(f.committed("payments"), 40);
        // And it is per tenant, not fleet-wide.
        assert_eq!(f.committed("platform"), 0);
    }

    #[test]
    fn the_lifetime_comes_from_outside() {
        // Per grant, because it comes from the tenant's policy and there is a policy
        // per tenant — the fleet could not own one number if it wanted to.
        let mut f = Fleet::default();
        f.arrive(worker("linux-1", &["linux"]));
        let a = f.allocate(&plea(&["linux"]), 1, "a", "dana", "dana", 30).unwrap();
        assert_eq!(a.expires_in, 30);
        assert_eq!(a.limits.max_seconds, 30);

        // A zero would expire every grant the instant it was made.
        f.release(&a.reservation, "a").unwrap();
        let b = f.allocate(&plea(&["linux"]), 1, "a", "dana", "dana", 0).unwrap();
        assert_eq!(b.expires_in, 1);
    }

    #[test]
    fn one_machine_serves_one_reservation() {
        // Concurrency belongs to the operating system: run more `cm worker`
        // processes. A machine advertising several slots made a rate ambiguous —
        // whose minute is being charged? — and left no moment between tenants for
        // the operator's cleanup to run in.
        let mut f = Fleet::default();
        f.arrive(worker("big", &["linux"]));
        assert!(f.allocate(&plea(&["linux"]), 1, "a", "dana", "dana", RESERVATION_SECS).is_ok());
        assert!(
            f.allocate(&plea(&["linux"]), 1, "b", "kiran", "kiran", RESERVATION_SECS).is_err(),
            "it is taken"
        );
        assert!(!f.workers().next().unwrap().is_free());
    }
}
