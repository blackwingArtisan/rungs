# Rungs: bounty payouts where trust is earned

Rungs is a team bounty board on NEAR. How much approval a payout needs depends on two things: how big it is, and how much trust the contributor has **earned**. Small payouts to proven contributors are instant. Medium ones need one council member. Big ones become a real proposal on the team's [Sputnik DAO](https://github.com/near-daos/sputnik-dao-contract), and the DAO's vote releases the money.

**Track:** Social Money

- Demo video: see the X post linked in the submission (also `video/rungs-demo.mp4` in the submission)
- Live board (testnet): [`board.rungs-ops-5860.testnet`](https://testnet.nearblocks.io/address/board.rungs-ops-5860.testnet)
- Team DAO (testnet): [`rungs5860.sputnikv2.testnet`](https://testnet.nearblocks.io/address/rungs5860.sputnikv2.testnet)
- Read-only dashboard: https://blackwingartisan.github.io/rungs/

## The problem

Teams, guilds and open-source projects pay people for small jobs every week: fix a bug, translate a page, review a PR. On-chain, they get two bad options:

- **Put every payout to a DAO vote.** A $20 task waits days for quorum. Contributors drift away.
- **Give someone a hot wallet.** Fast, but one person can pay anyone anything, including their own sock accounts.

Real teams don't work either way. A new contributor's first job gets checked; a regular's small fix gets paid on trust; a big contract goes to the whole group. Rungs puts that social rule on-chain.

## How it works

| Rung | When | Who signs |
|---|---|---|
| **1. Instant** | payout ≤ 0.5 NEAR **and** the contributor has ≥ 3 reputation **and** the team-wide daily instant cap has room | nobody: paid the moment the work is submitted |
| **2. One council approval** | payout ≤ 1 NEAR (or rung 1 doesn't apply) | one council member who is **not** the poster and **not** the claimant |
| **3. DAO vote** | payout > 1 NEAR | the board files a `Transfer` proposal on the team's Sputnik DAO; the DAO's council votes; the DAO treasury pays |

Flow: a council member `post`s a bounty → a contributor `claim`s it → `submit(id, proof_url)` routes it to a rung. Submitting is what decides the rung, so nobody can pick their own.

**Reputation is the only way up the ladder, so it's built to be hard to farm:**

- +1 per paid bounty (+2 for a DAO-approved one). It can't be transferred or bought.
- **Per-poster cap:** one poster can give one contributor at most 2 points, ever. Reaching rung 1 takes trust from at least 2 different council members.
- Nobody approves their own bounty or their own payout, and posters can't claim their own bounties.
- Only named accounts can claim. Implicit (64-hex) accounts are free to mint by the thousand, so they can't earn reputation.
- Points halve every 30 days. Trust has to be kept up.
- `dispute(id)`: any council member can flag a paid bounty within 7 days. It costs the contributor at least 3 points.
- **Daily instant cap:** even contributors with full reputation can only pull a fixed amount per day without review, team-wide. A compromised regular can't drain the board.

```
 contributor           Rungs board contract (testnet)                       Sputnik DAO
 ──────────            ─────────────────────────────────────────           ───────────
 claim, submit ──►  amount ≤ 0.5 and rep ≥ 3 and cap left? ── yes ─► pay now   (rung 1)
                         │ no
                    amount ≤ 1 ? ── yes ─► wait for council_approve ─► pay     (rung 2)
                         │ no
                    add_proposal(Transfer) ───────────────────────────► council votes
                    sync_dao(id) ◄─────────────── get_proposal: Approved ◄─ treasury pays (rung 3)
                         │
                    reputation: +1/+2, capped per poster, decays, −3 on dispute
```

## The demo (three acts, real testnet output)

`npm run demo` makes two brand-new contributor accounts and plays three acts against the live board and DAO:

```
== Act 1: a newcomer earns the instant tier
lead posts #72 "Fix typo in setup docs" for 0.2 NEAR
alice claims and submits #72 -> Council: reputation 0 < 3 needed for auto [AwaitingCouncil]
rev approves #72 -> Paid, +1 rep
...
rev posts #74 "Translate README to Spanish" for 0.2 NEAR
lead approves #74 -> Paid, +1 rep
alice reputation 3 (instant tier unlocked)
ops posts #75 "Fix broken link in footer" for 0.2 NEAR
alice claims and submits #75 -> Auto: reputation 3 >= 3: paid with no review [Paying]
#75 paid instantly, no human in the loop

== Act 2: one rogue poster farms a sock account
ops posts #76 "Rename a variable" for 0.2 NEAR
rev approves #76 -> Paid, +1 rep
...
rev approves #78 -> Paid, +0 rep          <- ops already gave the 2-point maximum
sock reputation 2
sock claims and submits #79 -> Council: reputation 2 < 3 needed for auto [AwaitingCouncil]
lead rejects #79

== Act 3: a big bounty goes to the DAO
rev posts #80 "Security audit of the payout flow" for 1.5 NEAR
alice claims and submits #80 -> Dao: over the council limit of 1 NEAR: DAO vote [AwaitingDao]
DAO proposal 6 on rungs5860.sputnikv2.testnet: "Rungs bounty #80: Security audit of the payout flow" status InProgress
lead votes approve -> InProgress
rev votes approve -> Approved
sync_dao -> Approved; board marks #80 Paid, +1 rep (rev already gave the 2-point maximum)
alice balance 1.2363 -> 2.7953 NEAR (paid by the DAO treasury)
```

The DAO step is a real cross-contract call: the board calls Sputnik's `add_proposal` with the 1 NEAR proposal bond attached, and a callback stores the proposal id (or rolls the bounty back if the DAO refuses). The DAO's own council votes with `act_proposal`. The money moves from the DAO treasury, not from the board.

## Try it: do X, see Y

You need Node 20+, Rust 1.86 with `wasm32-unknown-unknown`, [`cargo-near`](https://github.com/near/cargo-near) and the [`near` CLI](https://github.com/near/near-cli-rs).

```bash
# 1. Contract tests (12): newcomer -> council -> instant, the sock capped at 2 points,
#    DAO routing + rollback, daily cap overflow, decay, disputes, access rules
cd contract && cargo test
# → test result: ok. 12 passed

# 2. Build
cargo near build non-reproducible-wasm
# → target/near/rungs.wasm

# 3. Five funded testnet accounts from the faucet (TAG is any number you pick)
for r in lead rev ops alice sock; do
  near account create-account sponsor-by-faucet-service rungs-$r-1234.testnet \
    autogenerate-new-keypair save-to-legacy-keychain network-config testnet create
done

# 4. Deploy the board, create the team's Sputnik DAO (council: lead + rev) and fund it
cd ../app && npm install
TAG=1234 npm run setup
# → board budget ~4.6 NEAR, dao policy roles printed

# 5a. Play the three acts in the terminal
npm run demo
# 5b. Or watch it on the dashboard
npm start   # → http://localhost:8788, click "Run the 3-act demo"
```

Each demo run moves about 2 NEAR out of the board and 1.5 NEAR out of the DAO treasury. Top them up with `fund()` on the board and a plain transfer to the DAO. Keys stay in the NEAR CLI keychain (`~/.near-credentials/testnet`), never in the repo.

## Contract API

| Method | Who | Purpose |
|---|---|---|
| `new(owner, config)` | deployer | `config`: `{council, dao, auto_max, council_max, auto_rep, daily_auto_cap, per_poster_cap, decay_days, dao_bond}` (yoctoNEAR strings) |
| `set_config(config)` | owner | change council, limits or DAO |
| `fund()` | anyone, payable | top up the board's budget |
| `post(title, amount)` | council | returns the bounty id |
| `claim(id)` | any named account except the poster | |
| `submit(id, proof_url)` | the claimant | routes to a rung; returns `Paying`, `AwaitingCouncil` or `AwaitingDao` |
| `council_approve(id)` / `council_reject(id, reason?)` | council (not the poster or claimant) | settle a rung-2 payout |
| `sync_dao(id)` | anyone | reads the DAO proposal; marks the bounty `Paid` or `Rejected` |
| `dispute(id, reason)` | council, within 7 days | −3 reputation (at least), no clawback |
| `get_bounty`, `list_bounties(limit?)`, `reputation(account)`, `get_config`, `budget`, `auto_left_today` | view | |

Every step emits a NEP-297 event (`EVENT_JSON:{"standard":"rungs",...}`) for indexers and bots. A failed payout transfer is caught in a callback: the bounty goes back to the council, no reputation is granted, and the instant cap is restored.

## Limits we know about

- Two colluding council members can still pay a sock up to the council rung. Rungs makes that visible and slow (2 points per poster, disputes, decay); it doesn't make it impossible. Choosing an honest council stays a social job.
- `sync_dao` has to be called after the vote. A bot or the dashboard can do it; anyone may.
- The demo DAO uses Sputnik's default policy (council group, more than half must approve).

## What's next

- Fungible-token bounties (USDC on NEAR) alongside NEAR
- Contributors sign in with a wallet (wallet-selector) instead of scripts
- Reputation weighted by bounty size, and portable reputation other boards can read
- Auto-sync: a small keeper that calls `sync_dao` when a proposal closes

## Our other project

The same team also submitted **[Leash](https://github.com/blackwingArtisan/leash)** (AI Security & Defense, Agent Economy, Real-World Money): on-chain spending limits for AI agents. The two are independent codebases. Both happen to keep the rules about money in a contract instead of in someone's head.

## Disclosure (hackathon rules)

- **Built during NEAR HACKATHON, 2026-11-14.** All code in this repo was written during the hackathon. Nothing is pre-existing.
- **AI-generated code:** this project was built with Claude Code (Anthropic) as a coding assistant. The contract, scripts, dashboard and README were AI-assisted, then reviewed and run against NEAR testnet.
- **Libraries:** [`near-sdk`](https://crates.io/crates/near-sdk) 5.x (Rust contract), [`near-api-js`](https://www.npmjs.com/package/near-api-js) 7.x (scripts), [Sputnik DAO v2](https://github.com/near-daos/sputnik-dao-contract) (the existing testnet factory `sputnikv2.testnet`, called, not copied), `cargo-near` and `near-cli-rs` (build/deploy). Fonts: Inter and JetBrains Mono via Google Fonts. No templates.

## License

MIT © Stubborn Otter Software
