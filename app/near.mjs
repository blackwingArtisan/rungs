// Shared NEAR helpers for the setup and demo scripts.
import { Account, FailoverRpcProvider, JsonRpcProvider, KeyPair, nearToYocto, yoctoToNear } from 'near-api-js';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const HERE = path.dirname(fileURLToPath(import.meta.url));
// Free testnet RPCs rate-limit (HTTP 429), so fail over between several.
export const RPCS = (process.env.NEAR_RPC || 'https://near-testnet.drpc.org,https://rpc.testnet.fastnear.com,https://test.rpc.fastnear.com').split(',');
export const provider = new FailoverRpcProvider(RPCS.map((url) => new JsonRpcProvider({ url })));
export const DEPLOYMENT = path.join(HERE, 'deployment.json');
const KEYCHAIN = path.join(os.homedir(), '.near-credentials', 'testnet');

export const toYocto = (near) => BigInt(nearToYocto(String(near)));
export const toNear = (yocto) => Number(yoctoToNear(BigInt(yocto), 4));

export function loadDeployment() {
  if (!fs.existsSync(DEPLOYMENT)) throw new Error('No deployment.json yet: run `npm run setup` first.');
  return JSON.parse(fs.readFileSync(DEPLOYMENT, 'utf8'));
}

// Keys live in the NEAR CLI's legacy keychain (~/.near-credentials), never in the repo.
export function secretFor(accountId) {
  const file = path.join(KEYCHAIN, `${accountId}.json`);
  return JSON.parse(fs.readFileSync(file, 'utf8')).private_key;
}

export function saveKey(accountId, keyPair) {
  fs.mkdirSync(KEYCHAIN, { recursive: true });
  const file = path.join(KEYCHAIN, `${accountId}.json`);
  fs.writeFileSync(
    file,
    JSON.stringify({ account_id: accountId, public_key: keyPair.getPublicKey().toString(), private_key: keyPair.toString() }),
    { mode: 0o600 },
  );
}

export const newKey = () => KeyPair.fromRandom('ed25519');

export function accountFor(accountId) {
  return new Account(accountId, provider, secretFor(accountId));
}

// A rate-limited (429) request was refused before anything ran, so it's safe to retry.
async function retry429(fn) {
  for (let i = 1; ; i++) {
    try {
      return await fn();
    } catch (e) {
      if (i >= 6 || !/429|Rate limit/i.test(String(e?.message) + String(e?.cause))) throw e;
      await new Promise((r) => setTimeout(r, 1500 * i));
    }
  }
}

export function view(contractId, method, args = {}) {
  return retry429(() => provider.callFunction({ contractId, method, args }));
}

export function call(account, contractId, methodName, args = {}, deposit = 0n, tgas = 100) {
  return retry429(() => account.callFunction({ contractId, methodName, args, deposit, gas: BigInt(tgas) * 1_000_000_000_000n }));
}
