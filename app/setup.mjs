// One-time testnet setup: deploys the Rungs board, creates the team's Sputnik
// DAO (council = two of the board's council members) and funds both.
// Usage: TAG=5860 npm run setup   (expects rungs-{lead,rev,ops,alice,sock}-TAG.testnet
// in ~/.near-credentials/testnet, e.g. from the NEAR CLI faucet)
import fs from 'node:fs';
import path from 'node:path';
import { HERE, DEPLOYMENT, accountFor, call, newKey, saveKey, toYocto, toNear, view } from './near.mjs';

const tag = process.env.TAG;
if (!tag) throw new Error('Set TAG to the suffix of your rungs-*-TAG.testnet accounts');
const people = Object.fromEntries(['lead', 'rev', 'ops', 'alice', 'sock'].map((r) => [r, `rungs-${r}-${tag}.testnet`]));
const board = `board.${people.ops}`;
const dao = `rungs${tag}.sputnikv2.testnet`;

const ops = accountFor(people.ops);
const kp = newKey();
console.log(`creating ${board} with 7 NEAR`);
await ops.createSubAccount({ accountOrPrefix: 'board', publicKey: kp.getPublicKey(), nearToTransfer: toYocto('7') });
saveKey(board, kp);

const wasm = fs.readFileSync(path.join(HERE, '..', 'contract', 'target', 'near', 'rungs.wasm'));
const boardAcc = accountFor(board);
console.log(`deploying Rungs (${wasm.length} bytes)`);
await boardAcc.deployContract(new Uint8Array(wasm));

// Default Sputnik policy: the listed accounts form the council, anyone can propose
// (with a 1 NEAR bond), and a Transfer needs more than half the council.
console.log(`creating DAO ${dao}`);
const daoArgs = {
  config: { name: `rungs${tag}`, purpose: 'Rungs demo team treasury', metadata: '' },
  policy: [people.lead, people.rev],
};
await call(accountFor(people.lead), 'sputnikv2.testnet', 'create',
  { name: `rungs${tag}`, args: Buffer.from(JSON.stringify(daoArgs)).toString('base64') }, toYocto('6'), 300);
await accountFor(people.rev).transfer({ receiverId: dao, amount: toYocto('3') });

const near = (n) => toYocto(n).toString();
const config = {
  council: [people.lead, people.rev, people.ops],
  dao,
  auto_max: near('0.5'),
  council_max: near('1'),
  auto_rep: 3,
  daily_auto_cap: near('5'),
  per_poster_cap: 2,
  decay_days: 30,
  dao_bond: near('1'),
};
await call(boardAcc, board, 'new', { owner: people.lead, config });

const deployment = { network: 'testnet', board, dao, ...people };
fs.writeFileSync(DEPLOYMENT, JSON.stringify(deployment, null, 2) + '\n');
fs.writeFileSync(path.join(HERE, '..', 'web', 'deployment.json'), JSON.stringify(deployment, null, 2) + '\n');
console.log('board budget:', toNear(await view(board, 'budget')), 'NEAR');
console.log('dao policy roles:', (await view(dao, 'get_policy')).roles.map((r) => `${r.name}: ${JSON.stringify(r.kind)}`).join(' | '));
console.log(`wrote ${DEPLOYMENT}`);
