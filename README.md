# sorare-escrow

Anchor program holding SOL prize pools for Sorare contests.

## Instructions
| Instruction | Who | What |
|---|---|---|
| `create_pool(pool_id, entry_fee, deadline, max_players, fee_bps)` | organizer | Creates `Pool` PDA `["pool", organizer, pool_id_le_u64]`; vault PDA `["vault", pool]`. `fee_bps` ≤ 1000 (10%). Deadline must be in the future; entry fee ≥ rent-exempt minimum for a 0-byte account (890,880 lamports). |
| `deposit()` | player | Before deadline, while Open and not full: transfers exactly `entry_fee` to the vault, creates `PlayerEntry` PDA `["entry", pool, player]` (`init` ⇒ no double entry). |
| `settle(winner)` | organizer | Once, after the deadline, while Open. `winner_entry` PDA must exist for the winner (proves deposit). Vault balance minus `fee_bps` fee goes to the winner; fee goes to organizer. Vault ends at 0. |
| `cancel()` | organizer | Open → Cancelled (any time before settle). |
| `refund()` | player | When Cancelled: returns the player's deposit and closes the entry (rent back), so a second refund is impossible. |
| `close_pool()` | organizer | Closes the Pool account (rent back) if Settled, or Cancelled with every player refunded. |
| `close_entry()` | player | Reclaims entry rent after the pool is settled (or after the pool account is closed). |

Events: `PoolCreated`, `PlayerDeposited`, `PoolSettled`, `PoolCancelled`, `PlayerRefunded`, `PoolClosed`, `EntryClosed`.
Errors: see `EscrowError` in `programs/sorare-escrow/src/lib.rs`.

## Build & test locally
Requires Rust, Solana CLI (Agave) and Anchor 0.31.1 (`avm install 0.31.1 && avm use 0.31.1`).
```bash
yarn install
anchor keys sync      # writes a real program id into lib.rs and Anchor.toml
anchor build
anchor test           # spins up a local validator and runs tests/sorare-escrow.ts
```

## Deploy to devnet
```bash
solana config set --url devnet
solana-keygen new -o ~/.config/solana/id.json   # if you don't have a wallet
solana airdrop 2                                 # deploying needs ~2-3 SOL; use faucet.solana.com if rate limited
anchor keys sync
anchor build
anchor deploy --provider.cluster devnet
```
The program id is in `target/deploy/sorare_escrow-keypair.json`; keep it to upgrade later
(`anchor upgrade target/deploy/sorare_escrow.so --program-id <ID> --provider.cluster devnet`).
The IDL / TS types are in `target/idl` and `target/types`.

## Caveats
- The organizer is trusted to pick the correct winner; the program only checks the winner deposited.
- Settlement requires the deadline to have passed (so no one can join after the result is decided).
- Single winner only; no expiry if the organizer never settles or cancels (funds stay locked) — consider adding a permissionless timeout refund for production.
- Not audited.

## Running a pool end to end (`scripts/run-pool.ts`)
Creates a pool, funds N generated player wallets from the organizer wallet, deposits, waits for the
deadline, settles to a random player, closes the pool and entries, and sweeps leftover player SOL back.
Prints Explorer links for every transaction.
```bash
# organizer wallet needs ~2 SOL on devnet for the deploy plus ~0.1 SOL for the demo
# (devnet RPC airdrops are closed; use https://faucet.solana.com with GitHub sign-in)
anchor deploy --provider.cluster devnet
ANCHOR_PROVIDER_URL=https://api.devnet.solana.com ANCHOR_WALLET=~/.config/solana/id.json \
  yarn run-pool --players 3 --fee-sol 0.01 --deadline-secs 20 --fee-bps 500
```
