ests/sorare-escrow.tsimport * as anchor from "@coral-xyz/anchor";
import { Program, BN } from "@coral-xyz/anchor";
import { Keypair, LAMPORTS_PER_SOL, PublicKey, SystemProgram } from "@solana/web3.js";
import { expect } from "chai";
import { SorareEscrow } from "../target/types/sorare_escrow";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("sorare-escrow", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.SorareEscrow as Program<SorareEscrow>;
  const conn = provider.connection;
  const organizer = (provider.wallet as anchor.Wallet).payer;
  const FEE = new BN(0.1 * LAMPORTS_PER_SOL);
  let nextId = 1;

  const pda = (seeds: Buffer[]) => PublicKey.findProgramAddressSync(seeds, program.programId)[0];
  const poolPda = (id: BN) => pda([Buffer.from("pool"), organizer.publicKey.toBuffer(), id.toArrayLike(Buffer, "le", 8)]);
  const vaultPda = (pool: PublicKey) => pda([Buffer.from("vault"), pool.toBuffer()]);
  const entryPda = (pool: PublicKey, p: PublicKey) => pda([Buffer.from("entry"), pool.toBuffer(), p.toBuffer()]);

  async function fund(): Promise<Keypair> {
    const kp = Keypair.generate();
    const sig = await conn.requestAirdrop(kp.publicKey, 2 * LAMPORTS_PER_SOL);
    await conn.confirmTransaction(sig, "confirmed");
    return kp;
  }

  async function now(): Promise<number> {
    const slot = await conn.getSlot();
    return (await conn.getBlockTime(slot))!;
  }

  async function createPool(opts: { deadlineIn?: number; max?: number; feeBps?: number } = {}) {
    const id = new BN(nextId++);
    const pool = poolPda(id);
    const deadline = new BN((await now()) + (opts.deadlineIn ?? 6));
    await program.methods
      .createPool(id, FEE, deadline, opts.max ?? 10, opts.feeBps ?? 0)
      .accounts({ organizer: organizer.publicKey })
      .rpc();
    return { id, pool, vault: vaultPda(pool), deadline };
  }

  const deposit = (pool: PublicKey, p: Keypair) =>
    program.methods.deposit().accounts({ player: p.publicKey, pool } as any).signers([p]).rpc();

  async function expectErr(p: Promise<any>, code: string) {
    try { await p; } catch (e: any) {
      expect(String(e?.error?.errorCode?.code ?? e)).to.include(code);
      return;
    }
    expect.fail(`expected ${code}`);
  }

  async function waitPast(deadline: BN) {
    while ((await now()) <= deadline.toNumber()) await sleep(500);
  }

  it("rejects fee above 10%", async () => {
    const id = new BN(9999);
    await expectErr(
      program.methods.createPool(id, FEE, new BN((await now()) + 60), 5, 1001)
        .accounts({ organizer: organizer.publicKey }).rpc(),
      "FeeTooHigh");
  });

  it("full happy path with 5% organizer fee, settle, close", async () => {
    const { pool, vault, deadline } = await createPool({ feeBps: 500 });
    const [a, b] = [await fund(), await fund()];
    await deposit(pool, a);
    await deposit(pool, b);
    // double entry blocked by entry PDA init
    try { await deposit(pool, a); expect.fail("double entry"); } catch (e: any) {
      expect(String(e)).to.match(/already in use|0x0/);
    }
    const vaultBal = await conn.getBalance(vault);
    expect(vaultBal).to.equal(2 * FEE.toNumber());

    // cannot settle before deadline
    await expectErr(program.methods.settle(a.publicKey)
      .accounts({ organizer: organizer.publicKey, pool, winner: a.publicKey } as any).rpc(), "DeadlineNotReached");

    await waitPast(deadline);
    const late = await fund();
    await expectErr(deposit(pool, late), "DeadlinePassed");

    // non-depositor cannot win
    await expectErr(program.methods.settle(late.publicKey)
      .accounts({ organizer: organizer.publicKey, pool, winner: late.publicKey } as any).rpc(), "AccountNotInitialized");

    const before = await conn.getBalance(b.publicKey);
    await program.methods.settle(b.publicKey)
      .accounts({ organizer: organizer.publicKey, pool, winner: b.publicKey } as any).rpc();
    const after = await conn.getBalance(b.publicKey);
    const fee = Math.floor((vaultBal * 500) / 10000);
    expect(after - before).to.equal(vaultBal - fee);
    expect(await conn.getBalance(vault)).to.equal(0);
    const st = await program.account.pool.fetch(pool);
    expect(st.status).to.have.property("settled");

    // settle only once
    await expectErr(program.methods.settle(b.publicKey)
      .accounts({ organizer: organizer.publicKey, pool, winner: b.publicKey } as any).rpc(), "PoolNotOpen");

    await program.methods.closePool().accounts({ organizer: organizer.publicKey, pool } as any).rpc();
    expect(await conn.getAccountInfo(pool)).to.equal(null);
    await program.methods.closeEntry().accounts({ player: a.publicKey, pool } as any).signers([a]).rpc();
    expect(await conn.getAccountInfo(entryPda(pool, a.publicKey))).to.equal(null);
  });

  it("enforces max players and organizer-only settle", async () => {
    const { pool } = await createPool({ max: 1 });
    const [a, b] = [await fund(), await fund()];
    await deposit(pool, a);
    await expectErr(deposit(pool, b), "PoolFull");
    const imposter = await fund();
    try {
      await program.methods.cancel().accounts({ organizer: imposter.publicKey, pool } as any).signers([imposter]).rpc();
      expect.fail("imposter cancelled");
    } catch (e: any) { expect(String(e)).to.match(/Unauthorized|ConstraintSeeds|seeds/i); }
  });

  it("cancel + refunds, then close", async () => {
    const { pool, vault } = await createPool({ deadlineIn: 60 });
    const [a, b] = [await fund(), await fund()];
    await deposit(pool, a);
    await deposit(pool, b);
    await expectErr(program.methods.refund().accounts({ player: a.publicKey, pool } as any).signers([a]).rpc(), "PoolNotCancelled");
    await program.methods.cancel().accounts({ organizer: organizer.publicKey, pool } as any).rpc();
    await expectErr(deposit(pool, await fund()), "PoolNotOpen");

    const before = await conn.getBalance(a.publicKey);
    await program.methods.refund().accounts({ player: a.publicKey, pool } as any).signers([a]).rpc();
    expect((await conn.getBalance(a.publicKey)) - before).to.be.greaterThan(FEE.toNumber()); // fee + entry rent
    // double refund impossible: entry closed
    try { await program.methods.refund().accounts({ player: a.publicKey, pool } as any).signers([a]).rpc(); expect.fail(); }
    catch (e: any) { expect(String(e)).to.match(/AccountNotInitialized/); }

    await expectErr(program.methods.closePool().accounts({ organizer: organizer.publicKey, pool } as any).rpc(), "RefundsOutstanding");
    await program.methods.refund().accounts({ player: b.publicKey, pool } as any).signers([b]).rpc();
    expect(await conn.getBalance(vault)).to.equal(0);
    await program.methods.closePool().accounts({ organizer: organizer.publicKey, pool } as any).rpc();
  });
});
