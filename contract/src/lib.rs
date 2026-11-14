//! Rungs: a team bounty board where how much approval a payout needs
//! depends on its size and on trust the contributor has earned.
//!
//!   small + trusted contributor  -> paid instantly (Auto)
//!   up to the council limit       -> one council member approves (Council)
//!   above it                      -> a Transfer proposal on the team's Sputnik DAO (Dao)
//!
//! Reputation is the only way up the ladder, so it is built to be hard to farm:
//! only council members post bounties, nobody approves their own bounty or
//! their own payout, one poster can give one contributor at most
//! `per_poster_cap` points, points decay, and the Auto tier has a team-wide
//! daily cap.

use near_sdk::json_types::U128;
use near_sdk::serde_json::{json, Value};
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{
    env, ext_contract, log, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise,
    PromiseError,
};

const DAY_MS: u64 = 86_400_000;
const DISPUTE_WINDOW_MS: u64 = 7 * DAY_MS;
const DISPUTE_PENALTY: u32 = 3;
const RESERVE: u128 = 100_000_000_000_000_000_000_000; // 0.1 NEAR kept for storage
const GAS_DAO_CALL: Gas = Gas::from_tgas(50);
const GAS_CALLBACK: Gas = Gas::from_tgas(20);

#[ext_contract(ext_dao)]
pub trait SputnikDao {
    fn add_proposal(&mut self, proposal: Value) -> u64;
    fn get_proposal(&self, id: u64) -> Value;
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Config {
    pub council: Vec<AccountId>,
    /// The team's Sputnik DAO; large payouts become Transfer proposals there.
    pub dao: AccountId,
    pub auto_max: U128,
    pub council_max: U128,
    /// Reputation points needed before small payouts skip review.
    pub auto_rep: u32,
    pub daily_auto_cap: U128,
    /// Most points any single poster can ever give one contributor.
    pub per_poster_cap: u32,
    /// Points halve every this many days.
    pub decay_days: u32,
    /// Sputnik's proposal bond, attached when escalating.
    pub dao_bond: U128,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Open,
    Claimed,
    AwaitingCouncil,
    AwaitingDao,
    Paying,
    Paid,
    Rejected,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub enum Tier {
    Auto,
    Council,
    Dao,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Bounty {
    pub id: u64,
    pub poster: AccountId,
    pub title: String,
    pub amount: U128,
    pub status: Status,
    pub claimant: Option<AccountId>,
    pub proof: Option<String>,
    pub tier: Option<Tier>,
    pub approver: Option<AccountId>,
    pub dao_proposal: Option<u64>,
    pub paid_ms: Option<u64>,
    pub rep_granted: u32,
    pub disputed: bool,
    /// Why the bounty is where it is, in plain words.
    pub note: Option<String>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default)]
pub struct Rep {
    pub points: u32,
    pub updated_ms: u64,
    pub from_poster: Vec<(AccountId, u32)>,
}

#[near(serializers = [json])]
pub struct RepView {
    pub account: AccountId,
    pub points: u32,
    pub from_poster: Vec<(AccountId, u32)>,
    pub auto_eligible: bool,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Rungs {
    owner: AccountId,
    config: Config,
    bounties: Vector<Bounty>,
    reps: LookupMap<AccountId, Rep>,
    auto_day: u64,
    auto_spent_today: u128,
}

fn emit(event: &str, data: Value) {
    log!("EVENT_JSON:{}", json!({"standard": "rungs", "version": "1.0.0", "event": event, "data": [data]}));
}

fn near_str(yocto: u128) -> String {
    NearToken::from_yoctonear(yocto).exact_amount_display()
}

/// Implicit (64-hex) accounts are free to mint by the thousand; reputation is
/// only earned by named accounts.
fn is_implicit(a: &AccountId) -> bool {
    let s = a.as_str();
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())) || s.starts_with("0x")
}

impl Rep {
    fn decayed(&self, decay_days: u32, now_ms: u64) -> u32 {
        if decay_days == 0 || self.updated_ms == 0 {
            return self.points;
        }
        let periods = (now_ms.saturating_sub(self.updated_ms)) / (decay_days as u64 * DAY_MS);
        if periods >= 32 { 0 } else { self.points >> periods }
    }
}

#[near]
impl Rungs {
    #[init]
    pub fn new(owner: AccountId, config: Config) -> Self {
        require!(!config.council.is_empty(), "council can't be empty");
        Self {
            owner,
            config,
            bounties: Vector::new(b"b"),
            reps: LookupMap::new(b"r"),
            auto_day: 0,
            auto_spent_today: 0,
        }
    }

    pub fn set_config(&mut self, config: Config) {
        require!(env::predecessor_account_id() == self.owner, "only the owner can do this");
        require!(!config.council.is_empty(), "council can't be empty");
        self.config = config;
    }

    /// Anyone can top up the board's budget.
    #[payable]
    pub fn fund(&mut self) {}

    // ---------- bounty lifecycle ----------

    pub fn post(&mut self, title: String, amount: U128) -> u64 {
        let poster = env::predecessor_account_id();
        self.only_council(&poster);
        require!(amount.0 > 0, "amount must be above zero");
        if amount.0 <= self.config.council_max.0 {
            require!(amount.0 <= self.available(), "board budget too low for this bounty");
        }
        let id = self.bounties.len() as u64;
        self.bounties.push(Bounty {
            id,
            poster: poster.clone(),
            title: title.chars().take(120).collect(),
            amount,
            status: Status::Open,
            claimant: None,
            proof: None,
            tier: None,
            approver: None,
            dao_proposal: None,
            paid_ms: None,
            rep_granted: 0,
            disputed: false,
            note: None,
        });
        emit("bounty_posted", json!({"id": id, "poster": poster, "amount": amount}));
        id
    }

    pub fn claim(&mut self, id: u64) {
        let who = env::predecessor_account_id();
        require!(!is_implicit(&who), "claim from a named account; implicit accounts can't earn reputation");
        let b = self.bounty_mut(id);
        require!(b.status == Status::Open, "bounty is not open");
        require!(b.poster != who, "you can't claim your own bounty");
        b.claimant = Some(who.clone());
        b.status = Status::Claimed;
        emit("bounty_claimed", json!({"id": id, "claimant": who}));
    }

    /// Submitting the work is what routes the payout to a tier.
    pub fn submit(&mut self, id: u64, proof_url: String) -> Status {
        let who = env::predecessor_account_id();
        let now = env::block_timestamp_ms();
        self.roll_day(now);
        let (amount, decayed) = {
            let b = self.bounties.get(id as u32).expect("no such bounty");
            require!(b.status == Status::Claimed, "bounty is not waiting for work");
            require!(b.claimant.as_ref() == Some(&who), "only the claimant can submit");
            (b.amount.0, self.rep_points(&who, now))
        };
        let c = self.config.clone();
        let auto_ok = amount <= c.auto_max.0
            && decayed >= c.auto_rep
            && self.auto_spent_today + amount <= c.daily_auto_cap.0;
        let b = self.bounty_mut(id);
        b.proof = Some(proof_url.chars().take(300).collect());
        if auto_ok {
            b.tier = Some(Tier::Auto);
            b.note = Some(format!("reputation {decayed} >= {}: paid with no review", c.auto_rep));
            self.auto_spent_today += amount;
            self.pay(id);
            return Status::Paying;
        }
        if amount <= c.council_max.0 {
            b.tier = Some(Tier::Council);
            b.status = Status::AwaitingCouncil;
            b.note = Some(if amount > c.auto_max.0 {
                format!("over the auto limit of {}", near_str(c.auto_max.0))
            } else if decayed < c.auto_rep {
                format!("reputation {decayed} < {} needed for auto", c.auto_rep)
            } else {
                "team-wide daily auto cap reached".into()
            });
            emit("council_review", json!({"id": id, "reason": b.note}));
            return Status::AwaitingCouncil;
        }
        b.tier = Some(Tier::Dao);
        b.status = Status::AwaitingDao;
        b.note = Some(format!("over the council limit of {}: DAO vote", near_str(c.council_max.0)));
        let claimant = who;
        let proposal = json!({
            "description": format!("Rungs bounty #{id}: {}", b.title),
            "kind": {"Transfer": {"token_id": "", "receiver_id": claimant, "amount": U128(amount)}}
        });
        let _ = ext_dao::ext(c.dao.clone())
            .with_attached_deposit(NearToken::from_yoctonear(c.dao_bond.0))
            .with_static_gas(GAS_DAO_CALL)
            .add_proposal(proposal)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_proposal(id));
        Status::AwaitingDao
    }

    pub fn council_approve(&mut self, id: u64) {
        let who = env::predecessor_account_id();
        self.only_council(&who);
        let b = self.bounty_mut(id);
        require!(b.status == Status::AwaitingCouncil, "bounty is not waiting for the council");
        require!(b.poster != who, "the poster can't approve their own bounty");
        require!(b.claimant.as_ref() != Some(&who), "you can't approve your own payout");
        b.approver = Some(who.clone());
        b.note = Some(format!("approved by {who}"));
        emit("council_approved", json!({"id": id, "approver": who}));
        self.pay(id);
    }

    pub fn council_reject(&mut self, id: u64, reason: Option<String>) {
        let who = env::predecessor_account_id();
        self.only_council(&who);
        let b = self.bounty_mut(id);
        require!(b.status == Status::AwaitingCouncil, "bounty is not waiting for the council");
        b.status = Status::Rejected;
        b.approver = Some(who);
        b.note = Some(reason.unwrap_or_else(|| "rejected by council".into()));
        emit("bounty_rejected", json!({"id": id}));
    }

    /// Anyone can ask the board to read the DAO's verdict on an escalated bounty.
    pub fn sync_dao(&mut self, id: u64) -> Promise {
        let b = self.bounties.get(id as u32).expect("no such bounty");
        require!(b.status == Status::AwaitingDao, "bounty is not waiting for the DAO");
        let pid = b.dao_proposal.expect("proposal not created yet");
        ext_dao::ext(self.config.dao.clone())
            .with_static_gas(Gas::from_tgas(10))
            .get_proposal(pid)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_dao_status(id))
    }

    /// A council member can dispute a payout for 7 days. It costs the
    /// contributor reputation; it doesn't claw back funds.
    pub fn dispute(&mut self, id: u64, reason: String) {
        let who = env::predecessor_account_id();
        self.only_council(&who);
        let now = env::block_timestamp_ms();
        let (claimant, granted) = {
            let b = self.bounty_mut(id);
            require!(b.status == Status::Paid && !b.disputed, "only an undisputed paid bounty can be disputed");
            require!(now <= b.paid_ms.unwrap_or(0) + DISPUTE_WINDOW_MS, "dispute window closed");
            require!(b.claimant.as_ref() != Some(&who), "you can't dispute your own payout");
            b.disputed = true;
            b.note = Some(format!("disputed: {}", reason.chars().take(200).collect::<String>()));
            (b.claimant.clone().unwrap(), b.rep_granted)
        };
        let decay = self.config.decay_days;
        let mut rep = self.reps.get(&claimant).cloned().unwrap_or_default();
        rep.points = rep.decayed(decay, now).saturating_sub(DISPUTE_PENALTY.max(granted));
        rep.updated_ms = now;
        self.reps.insert(claimant.clone(), rep);
        emit("payout_disputed", json!({"id": id, "claimant": claimant}));
    }

    // ---------- callbacks ----------

    #[private]
    pub fn on_proposal(&mut self, id: u64, #[callback_result] result: Result<u64, PromiseError>) {
        let b = self.bounty_mut(id);
        match result {
            Ok(pid) => {
                b.dao_proposal = Some(pid);
                emit("dao_proposal_created", json!({"id": id, "proposal": pid}));
            }
            Err(_) => {
                b.status = Status::Claimed;
                b.tier = None;
                b.note = Some("DAO proposal failed; submit again".into());
            }
        }
    }

    #[private]
    pub fn on_dao_status(&mut self, id: u64, #[callback_result] result: Result<Value, PromiseError>) -> String {
        let Ok(p) = result else { return "dao unreachable".into() };
        let status = p.get("status").and_then(|s| s.as_str()).unwrap_or("Unknown").to_string();
        match status.as_str() {
            "Approved" => {
                let now = env::block_timestamp_ms();
                let b = self.bounty_mut(id);
                b.status = Status::Paid;
                b.paid_ms = Some(now);
                b.note = Some("paid by DAO vote".into());
                self.grant_rep(id, now);
                emit("bounty_paid", json!({"id": id, "tier": "Dao"}));
            }
            "Rejected" | "Removed" | "Expired" | "Failed" => {
                let b = self.bounty_mut(id);
                b.status = Status::Rejected;
                b.note = Some(format!("DAO proposal {status}"));
            }
            _ => {}
        }
        status
    }

    #[private]
    pub fn on_paid(&mut self, id: u64, #[callback_result] result: Result<(), PromiseError>) {
        let now = env::block_timestamp_ms();
        if result.is_ok() {
            let b = self.bounty_mut(id);
            b.status = Status::Paid;
            b.paid_ms = Some(now);
            let tier = b.tier.clone();
            self.grant_rep(id, now);
            emit("bounty_paid", json!({"id": id, "tier": tier}));
        } else {
            let auto = {
                let b = self.bounty_mut(id);
                b.status = Status::AwaitingCouncil;
                b.note = Some("transfer failed; waiting for the council".into());
                if b.tier == Some(Tier::Auto) { Some(b.amount.0) } else { None }
            };
            if let Some(a) = auto {
                self.auto_spent_today = self.auto_spent_today.saturating_sub(a);
            }
        }
    }

    // ---------- views ----------

    pub fn get_config(&self) -> Config {
        self.config.clone()
    }

    pub fn get_bounty(&self, id: u64) -> Option<Bounty> {
        self.bounties.get(id as u32).cloned()
    }

    /// Newest first.
    pub fn list_bounties(&self, limit: Option<u32>) -> Vec<Bounty> {
        let n = self.bounties.len();
        (0..n).rev().take(limit.unwrap_or(50).min(200) as usize).filter_map(|i| self.bounties.get(i).cloned()).collect()
    }

    pub fn reputation(&self, account: AccountId) -> RepView {
        let now = env::block_timestamp_ms();
        let rep = self.reps.get(&account).cloned().unwrap_or_default();
        let points = rep.decayed(self.config.decay_days, now);
        RepView { account, points, from_poster: rep.from_poster, auto_eligible: points >= self.config.auto_rep }
    }

    pub fn auto_left_today(&self) -> U128 {
        let today = env::block_timestamp_ms() / DAY_MS;
        let spent = if today == self.auto_day { self.auto_spent_today } else { 0 };
        U128(self.config.daily_auto_cap.0.saturating_sub(spent))
    }

    pub fn budget(&self) -> U128 {
        U128(self.available())
    }
}

impl Rungs {
    fn only_council(&self, who: &AccountId) {
        require!(self.config.council.contains(who), "only council members can do this");
    }

    fn bounty_mut(&mut self, id: u64) -> &mut Bounty {
        self.bounties.get_mut(id as u32).expect("no such bounty")
    }

    fn available(&self) -> u128 {
        let storage = env::storage_byte_cost().as_yoctonear() * env::storage_usage() as u128;
        env::account_balance().as_yoctonear().saturating_sub(storage + RESERVE)
    }

    fn roll_day(&mut self, now: u64) {
        let d = now / DAY_MS;
        if d != self.auto_day {
            self.auto_day = d;
            self.auto_spent_today = 0;
        }
    }

    fn rep_points(&self, who: &AccountId, now: u64) -> u32 {
        self.reps.get(who).map(|r| r.decayed(self.config.decay_days, now)).unwrap_or(0)
    }

    fn pay(&mut self, id: u64) {
        let available = self.available();
        let b = self.bounty_mut(id);
        require!(b.amount.0 <= available, "board budget too low; top it up with fund()");
        b.status = Status::Paying;
        let (to, amount) = (b.claimant.clone().unwrap(), b.amount.0);
        let _ = Promise::new(to)
            .transfer(NearToken::from_yoctonear(amount))
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_paid(id));
    }

    /// +1 point per paid bounty (+2 for a DAO-approved one), capped per poster.
    fn grant_rep(&mut self, id: u64, now: u64) {
        let (claimant, poster, tier) = {
            let b = self.bounties.get(id as u32).unwrap();
            (b.claimant.clone().unwrap(), b.poster.clone(), b.tier.clone())
        };
        let cap = self.config.per_poster_cap;
        let decay = self.config.decay_days;
        let mut rep = self.reps.get(&claimant).cloned().unwrap_or_default();
        rep.points = rep.decayed(decay, now);
        rep.updated_ms = now;
        let want: u32 = if tier == Some(Tier::Dao) { 2 } else { 1 };
        let given = rep.from_poster.iter().find(|(p, _)| *p == poster).map(|(_, n)| *n).unwrap_or(0);
        let grant = want.min(cap.saturating_sub(given));
        if grant > 0 {
            rep.points += grant;
            match rep.from_poster.iter_mut().find(|(p, _)| *p == poster) {
                Some((_, n)) => *n += grant,
                None => rep.from_poster.push((poster.clone(), grant)),
            }
        }
        self.reps.insert(claimant.clone(), rep);
        let b = self.bounty_mut(id);
        b.rep_granted = grant;
        if grant < want {
            b.note = Some(format!("{}; {poster} already gave the {cap}-point maximum", b.note.clone().unwrap_or_else(|| "paid".into())));
        }
        emit("reputation", json!({"account": claimant, "granted": grant, "poster": poster}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;

    const NEAR: u128 = 1_000_000_000_000_000_000_000_000;

    fn acc(s: &str) -> AccountId {
        s.parse().unwrap()
    }

    fn ctx(caller: &str, ts_ms: u64) {
        let mut b = VMContextBuilder::new();
        b.current_account_id(acc("board.testnet"))
            .predecessor_account_id(acc(caller))
            .account_balance(NearToken::from_near(20))
            .block_timestamp(ts_ms * 1_000_000);
        testing_env!(b.build());
    }

    fn setup() -> Rungs {
        ctx("lead.testnet", 1_000);
        Rungs::new(
            acc("lead.testnet"),
            Config {
                council: vec![acc("lead.testnet"), acc("rev.testnet"), acc("ops.testnet")],
                dao: acc("team.sputnikv2.testnet"),
                auto_max: U128(NEAR / 2),
                council_max: U128(NEAR),
                auto_rep: 3,
                daily_auto_cap: U128(NEAR),
                per_poster_cap: 2,
                decay_days: 30,
                dao_bond: U128(NEAR),
            },
        )
    }

    /// Posts, claims and submits; returns the status submit routed it to.
    fn run(c: &mut Rungs, poster: &str, who: &str, amount: u128, t: u64) -> (u64, Status) {
        ctx(poster, t);
        let id = c.post("task".into(), U128(amount));
        ctx(who, t + 1);
        c.claim(id);
        let s = c.submit(id, "https://github.com/x/pull/1".into());
        (id, s)
    }

    fn settle(c: &mut Rungs, id: u64, t: u64) {
        ctx("board.testnet", t);
        c.on_paid(id, Ok(()));
    }

    fn approve(c: &mut Rungs, approver: &str, id: u64, t: u64) {
        ctx(approver, t);
        c.council_approve(id);
        settle(c, id, t + 1);
    }

    #[test]
    fn newcomer_needs_council_then_earns_auto() {
        let mut c = setup();
        let mut t = 2_000;
        // three council-approved bounties from two different posters
        for (poster, approver) in [("lead.testnet", "rev.testnet"), ("lead.testnet", "rev.testnet"), ("rev.testnet", "lead.testnet")] {
            let (id, s) = run(&mut c, poster, "alice.testnet", NEAR / 4, t);
            assert_eq!(s, Status::AwaitingCouncil);
            approve(&mut c, approver, id, t + 5);
            t += 100;
        }
        assert_eq!(c.reputation(acc("alice.testnet")).points, 3);
        let (id, s) = run(&mut c, "ops.testnet", "alice.testnet", NEAR / 4, t);
        assert_eq!(s, Status::Paying);
        settle(&mut c, id, t + 5);
        let b = c.get_bounty(id).unwrap();
        assert_eq!(b.status, Status::Paid);
        assert_eq!(b.tier, Some(Tier::Auto));
    }

    #[test]
    fn one_rogue_poster_cannot_mint_auto_trust() {
        let mut c = setup();
        let mut t = 2_000;
        // lead posts 5 bounties for a sock; rev approves them all
        for _ in 0..5 {
            let (id, s) = run(&mut c, "lead.testnet", "sock.testnet", NEAR / 4, t);
            assert_eq!(s, Status::AwaitingCouncil, "sock never reaches auto");
            approve(&mut c, "rev.testnet", id, t + 5);
            t += 100;
        }
        let r = c.reputation(acc("sock.testnet"));
        assert_eq!(r.points, 2);
        assert!(!r.auto_eligible);
    }

    #[test]
    fn mid_size_always_needs_council_and_big_goes_to_dao() {
        let mut c = setup();
        let (_, s) = run(&mut c, "lead.testnet", "alice.testnet", NEAR * 3 / 4, 2_000);
        assert_eq!(s, Status::AwaitingCouncil);
        let (id, s) = run(&mut c, "lead.testnet", "bob.testnet", NEAR * 3, 3_000);
        assert_eq!(s, Status::AwaitingDao);
        ctx("board.testnet", 3_100);
        c.on_proposal(id, Ok(7));
        assert_eq!(c.get_bounty(id).unwrap().dao_proposal, Some(7));
        c.on_dao_status(id, Ok(json!({"status": "Approved"})));
        let b = c.get_bounty(id).unwrap();
        assert_eq!(b.status, Status::Paid);
        assert_eq!(c.reputation(acc("bob.testnet")).points, 2);
    }

    #[test]
    fn failed_dao_proposal_rolls_back() {
        let mut c = setup();
        let (id, _) = run(&mut c, "lead.testnet", "bob.testnet", NEAR * 3, 2_000);
        ctx("board.testnet", 2_100);
        c.on_proposal(id, Err(PromiseError::Failed));
        assert_eq!(c.get_bounty(id).unwrap().status, Status::Claimed);
    }

    #[test]
    fn daily_auto_cap_sends_overflow_to_council() {
        let mut c = setup();
        let mut t = 2_000;
        for (poster, approver) in [("lead.testnet", "rev.testnet"), ("lead.testnet", "rev.testnet"), ("rev.testnet", "lead.testnet")] {
            let (id, _) = run(&mut c, poster, "alice.testnet", NEAR / 10, t);
            approve(&mut c, approver, id, t + 5);
            t += 100;
        }
        // cap is 1 NEAR/day: two 0.45 auto payouts fit, the third doesn't
        for expect in [Status::Paying, Status::Paying, Status::AwaitingCouncil] {
            let (id, s) = run(&mut c, "ops.testnet", "alice.testnet", NEAR * 45 / 100, t);
            assert_eq!(s, expect);
            if s == Status::Paying { settle(&mut c, id, t + 5); }
            t += 100;
        }
    }

    #[test]
    fn reputation_decays() {
        let mut c = setup();
        let mut t = 2_000;
        for (poster, approver) in [("lead.testnet", "rev.testnet"), ("lead.testnet", "rev.testnet"), ("rev.testnet", "lead.testnet")] {
            let (id, _) = run(&mut c, poster, "alice.testnet", NEAR / 10, t);
            approve(&mut c, approver, id, t + 5);
            t += 100;
        }
        ctx("x.testnet", t + 30 * DAY_MS);
        assert_eq!(c.reputation(acc("alice.testnet")).points, 1);
    }

    #[test]
    fn dispute_costs_reputation() {
        let mut c = setup();
        let (id, _) = run(&mut c, "lead.testnet", "alice.testnet", NEAR / 10, 2_000);
        approve(&mut c, "rev.testnet", id, 2_100);
        ctx("ops.testnet", 3_000);
        c.dispute(id, "work was copied".into());
        assert_eq!(c.reputation(acc("alice.testnet")).points, 0);
    }

    #[test]
    #[should_panic(expected = "poster can't approve")]
    fn poster_cannot_approve_own_bounty() {
        let mut c = setup();
        let (id, _) = run(&mut c, "lead.testnet", "alice.testnet", NEAR / 4, 2_000);
        ctx("lead.testnet", 2_100);
        c.council_approve(id);
    }

    #[test]
    #[should_panic(expected = "can't claim your own bounty")]
    fn poster_cannot_claim_own_bounty() {
        let mut c = setup();
        ctx("lead.testnet", 2_000);
        let id = c.post("task".into(), U128(NEAR / 4));
        c.claim(id);
    }

    #[test]
    #[should_panic(expected = "only council members")]
    fn outsiders_cannot_post() {
        let mut c = setup();
        ctx("random.testnet", 2_000);
        c.post("task".into(), U128(NEAR / 4));
    }

    #[test]
    #[should_panic(expected = "implicit accounts")]
    fn implicit_accounts_cannot_claim() {
        let mut c = setup();
        ctx("lead.testnet", 2_000);
        let id = c.post("task".into(), U128(NEAR / 4));
        ctx("a96ad3cb539b653e4b869bd7cf26590690e8971de87d98bae20dfaf1f8a2ef3e", 2_100);
        c.claim(id);
    }

    #[test]
    fn failed_transfer_does_not_grant_reputation() {
        let mut c = setup();
        let (id, _) = run(&mut c, "lead.testnet", "alice.testnet", NEAR / 4, 2_000);
        ctx("rev.testnet", 2_100);
        c.council_approve(id);
        ctx("board.testnet", 2_200);
        c.on_paid(id, Err(PromiseError::Failed));
        assert_eq!(c.get_bounty(id).unwrap().status, Status::AwaitingCouncil);
        assert_eq!(c.reputation(acc("alice.testnet")).points, 0);
    }
}
