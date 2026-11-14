// Runs the three-act demo on testnet against the deployed board.
//   1. A newcomer's small bounties need a council click; after 3 points from
//      2 different posters, the same size pays out instantly.
//   2. A rogue council member feeds bounties to a sock account: the sock tops
//      out at 2 points and never reaches the instant tier.
//   3. A big bounty becomes a live Sputnik DAO proposal; two votes release it.
// Usage: npm run demo   (ACT=1|2|3 runs one act; PACE=ms between steps)
import { accountFor, call, loadDeployment, newKey, saveKey, toNear, toYocto, view } from './near.mjs';

const d = loadDeployment();
// Each run uses brand-new contributor accounts, so the newcomer really starts at 0.
const run = Date.now().toString(36).slice(-5);
for (const who of ['alice', 'sock']) {
  const kp = newKey();
  const id = `${who}-${run}.${d[who]}`;
  await accountFor(d[who]).createSubAccount({ accountOrPrefix: `${who}-${run}`, publicKey: kp.getPublicKey(), nearToTransfer: toYocto('0.5') });
  saveKey(id, kp);
  d[who] = id;
}
console.log(`fresh contributors: ${d.alice}, ${d.sock}`);
const PACE = Number(process.env.PACE || 0);
const ACT = process.env.ACT;
const acc = Object.fromEntries(['lead', 'rev', 'ops', 'alice', 'sock'].map((r) => [r, accountFor(d[r])]));
const pause = () => new Promise((r) => setTimeout(r, PACE));
const say = (s) => console.log(`\n== ${s}`);

async function bounty(id) {
  return view(d.board, 'get_bounty', { id });
}

// Payout callbacks land a block or two after the call returns, and RPC nodes
// can lag, so wait until the bounty has moved past `from`.
async function settled(id, from) {
  for (let i = 0; i < 12; i++) {
    const b = await bounty(id);
    if (!from.includes(b.status)) return b;
    await new Promise((r) => setTimeout(r, 1000));
  }
  return bounty(id);
}

async function rep(who) {
  const r = await view(d.board, 'reputation', { account: d[who] });
  return `${who} reputation ${r.points}${r.auto_eligible ? ' (instant tier unlocked)' : ''}`;
}

// post -> claim -> submit; returns the bounty after submit has routed it
async function work(poster, who, title, amountNear) {
  const id = await call(acc[poster], d.board, 'post', { title, amount: toYocto(amountNear).toString() });
  console.log(`${poster} posts #${id} "${title}" for ${amountNear} NEAR`);
  await pause();
  await call(acc[who], d.board, 'claim', { id });
  const status = await call(acc[who], d.board, 'submit', { id, proof_url: `https://github.com/blackwingArtisan/rungs/pull/${id}` }, 0n, 150);
  const b = await settled(id, ['Open', 'Claimed', 'Paying']);
  console.log(`${who} claims and submits #${id} -> ${b.tier}: ${b.note} [${status}]`);
  await pause();
  return b;
}

async function approve(approver, id) {
  await call(acc[approver], d.board, 'council_approve', { id });
  const b = await settled(id, ['AwaitingCouncil', 'Paying']);
  console.log(`${approver} approves #${id} -> ${b.status}${b.rep_granted ? `, +${b.rep_granted} rep` : ', +0 rep'}${/cap/.test(b.note) ? ' (per-poster cap)' : ''}`);
  await pause();
}

async function act1() {
  say('Act 1: a newcomer earns the instant tier');
  for (const [poster, approver, title] of [
    ['lead', 'rev', 'Fix typo in setup docs'],
    ['lead', 'rev', 'Add CI status badge'],
    ['rev', 'lead', 'Translate README to Spanish'],
  ]) {
    const b = await work(poster, 'alice', title, '0.2');
    await approve(approver, b.id);
    console.log(await rep('alice'));
  }
  const b = await work('ops', 'alice', 'Fix broken link in footer', '0.2');
  console.log(`#${b.id} ${b.status === 'Paid' ? 'paid instantly' : b.status}, no human in the loop`);
}

async function act2() {
  say('Act 2: one rogue poster farms a sock account');
  for (const title of ['Rename a variable', 'Bump a version number', 'Delete a blank line']) {
    const b = await work('ops', 'sock', title, '0.2');
    await approve('rev', b.id);
    console.log(await rep('sock'));
  }
  const b = await work('ops', 'sock', 'Another trivial task', '0.2');
  console.log(`sock still needs a human: ${b.status}`);
  await call(acc.lead, d.board, 'council_reject', { id: b.id, reason: 'pattern of trivial self-dealt bounties' });
  console.log(`lead rejects #${b.id}`);
}

async function act3() {
  say('Act 3: a big bounty goes to the DAO');
  const b = await work('rev', 'alice', 'Security audit of the payout flow', '1.5');
  let pid = b.dao_proposal;
  for (let i = 0; pid == null && i < 10; i++) {
    await new Promise((r) => setTimeout(r, 1500));
    pid = (await bounty(b.id)).dao_proposal;
  }
  const p = await view(d.dao, 'get_proposal', { id: pid });
  console.log(`DAO proposal ${pid} on ${d.dao}: "${p.description}" status ${p.status}`);
  await pause();
  const before = toNear((await acc.alice.getBalance()).toString());
  for (const voter of ['lead', 'rev']) {
    // skip a vote that already landed (e.g. an RPC hiccup after the transaction went through)
    const cur = await view(d.dao, 'get_proposal', { id: pid });
    if (!cur.votes[d[voter]]) await call(acc[voter], d.dao, 'act_proposal', { id: pid, action: 'VoteApprove', proposal: p.kind }, 0n, 200);
    const now = await view(d.dao, 'get_proposal', { id: pid });
    console.log(`${voter} votes approve -> ${now.status}`);
    await pause();
  }
  const result = await call(acc.ops, d.board, 'sync_dao', { id: b.id }, 0n, 60);
  const after = toNear((await acc.alice.getBalance()).toString());
  const done = await bounty(b.id);
  console.log(`sync_dao -> ${result}; board marks #${b.id} ${done.status}, +${done.rep_granted} rep (${done.note})`);
  console.log(`alice balance ${before} -> ${after} NEAR (paid by the DAO treasury)`);
  console.log(await rep('alice'));
}

if (!ACT || ACT === '1') await act1();
if (!ACT || ACT === '2') await act2();
if (!ACT || ACT === '3') await act3();
console.log(`\nboard budget left: ${toNear(await view(d.board, 'budget'))} NEAR`);
