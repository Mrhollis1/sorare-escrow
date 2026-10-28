/**
 * End-to-end pool demo / runner.
 *   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com ANCHOR_WALLET=~/.config/solana/id.json \
 *   npx ts-node scripts/run-pool.ts [--players 3] [--fee-sol 0.01] [--deadline-secs 20] [--fee-bps 500]
 * Players are generated wallets funded by the organizer wallet (no airdrops needed).
 */
import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import { Keypair, LAMPORTS_PER_SOL, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";
import idl from "../target/idl/sorare_escrow.json";
import { SorareEscrow } from "../target/types/sorare_escrow";

const arg = (k: string, d: number) => { const i = process.argv.indexOf(`--${k}`); return i > 0 ? Number(process.argv[i + 1]) : d; };
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new Program<SorareEscrow>(idl as any, provider);
  const conn = provider.connection;
  const org = (provider.wallet as anchor.Wallet).payer;
  const url = conn.rpcEndpoint;
  const cluster = url.includes("devnet") ? "devnet" : `custom&customUrl=${encodeURIComponent(url)}`;
  const tx = (s: string) => `https://explorer.solana.com/tx/${s}?cluster=${cluster}`;

  const nPlayers = arg("players", 3), feeBps = arg("fee-bps", 500), secs = arg("deadline-secs", 20);
  const entryFee = new BN(Math.round(arg("fee-sol", 0.01) * LAMPORTS_PER_SOL));
  const poolId = new BN(Date.now());
  const [pool] = PublicKey.findProgramAddressSync(
    [Buffer.from("pool"), org.publicKey.toBuffer(), poolId.toArrayLike(Buffer, "le", 8)], program.programId);
  const [vault] = PublicKey.findProgramAddressSync([Buffer.from("vault"), pool.toBuffer()], program.programId);
  console.log("program", program.programId.toBase58(), "\npool", pool.toBase58(), "\nvault", vault.toBase58());

  const blockTime = (await conn.getBlockTime(await conn.getSlot()))!;
  const deadline = new BN(blockTime + secs);
  console.log("create_pool", tx(await program.methods.createPool(poolId, entryFee, deadline, 10, feeBps)
    .accounts({ organizer: org.publicKey }).rpc()));

  const players = Array.from({ length: nPlayers }, () => Keypair.generate());
  const fund = new Transaction();
  for (const p of players) fund.add(SystemProgram.transfer({ fromPubkey: org.publicKey, toPubkey: p.publicKey,
    lamports: entryFee.toNumber() + 5_000_000 }));
  console.log("fund players", tx(await provider.sendAndConfirm(fund)));

  for (const p of players) console.log(`deposit ${p.publicKey.toBase58()}`, tx(await program.methods.deposit()
    .accounts({ player: p.publicKey, pool } as any).signers([p]).rpc()));
  console.log("vault balance", (await conn.getBalance(vault)) / LAMPORTS_PER_SOL, "SOL");

  while ((await conn.getBlockTime(await conn.getSlot()))! <= deadline.toNumber()) await sleep(2000);
  const winner = players[Math.floor(Math.random() * players.length)];
  const before = await conn.getBalance(winner.publicKey);
  console.log("settle ->", winner.publicKey.toBase58(), tx(await program.methods.settle(winner.publicKey)
    .accounts({ organizer: org.publicKey, pool, winner: winner.publicKey } as any).rpc()));
  console.log("winner received", ((await conn.getBalance(winner.publicKey)) - before) / LAMPORTS_PER_SOL, "SOL");

  console.log("close_pool", tx(await program.methods.closePool().accounts({ organizer: org.publicKey, pool } as any).rpc()));
  for (const p of players) await program.methods.closeEntry().accounts({ player: p.publicKey, pool } as any).signers([p]).rpc();
  // sweep leftover player SOL back to the organizer
  for (const p of players) {
    const bal = await conn.getBalance(p.publicKey);
    if (bal > 5000) await anchor.web3.sendAndConfirmTransaction(conn, new Transaction().add(SystemProgram.transfer({
      fromPubkey: p.publicKey, toPubkey: org.publicKey, lamports: bal - 5000 })), [p]);
  }
  console.log("done: entries closed, player funds swept back to organizer");
}
main().catch((e) => { console.error(e); process.exit(1); });
