//! Sorare contest prize-pool escrow (SOL).
use anchor_lang::prelude::*;
use anchor_lang::system_program::{self, Transfer};

declare_id!("ASqaMYaMhWKksUX4Tu54vAigRVaNFqRcXNQFUteKZp96");

pub const MAX_FEE_BPS: u16 = 1_000; // 10%
pub const POOL_SEED: &[u8] = b"pool";
pub const VAULT_SEED: &[u8] = b"vault";
pub const ENTRY_SEED: &[u8] = b"entry";

#[program]
pub mod sorare_escrow {
    use super::*;

    pub fn create_pool(
        ctx: Context<CreatePool>,
        pool_id: u64,
        entry_fee: u64,
        deadline: i64,
        max_players: u32,
        fee_bps: u16,
    ) -> Result<()> {
        // Vault is a 0-data system account: every non-zero balance it holds must
        // be rent-exempt, so the entry fee must cover the 0-byte rent minimum.
        require!(
            entry_fee >= Rent::get()?.minimum_balance(0),
            EscrowError::InvalidEntryFee
        );
        require!(max_players >= 1, EscrowError::InvalidMaxPlayers);
        require!(fee_bps <= MAX_FEE_BPS, EscrowError::FeeTooHigh);
        let now = Clock::get()?.unix_timestamp;
        require!(deadline > now, EscrowError::DeadlineInPast);

        let pool = &mut ctx.accounts.pool;
        pool.organizer = ctx.accounts.organizer.key();
        pool.pool_id = pool_id;
        pool.entry_fee = entry_fee;
        pool.deadline = deadline;
        pool.max_players = max_players;
        pool.player_count = 0;
        pool.refunded_count = 0;
        pool.fee_bps = fee_bps;
        pool.status = PoolStatus::Open;
        pool.winner = Pubkey::default();
        pool.bump = ctx.bumps.pool;
        pool.vault_bump = ctx.bumps.vault;

        emit!(PoolCreated {
            pool: pool.key(),
            organizer: pool.organizer,
            pool_id,
            entry_fee,
            deadline,
            max_players,
            fee_bps,
        });
        Ok(())
    }

    pub fn deposit(ctx: Context<Deposit>) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        require!(pool.status == PoolStatus::Open, EscrowError::PoolNotOpen);
        let now = Clock::get()?.unix_timestamp;
        require!(now < pool.deadline, EscrowError::DeadlinePassed);
        require!(pool.player_count < pool.max_players, EscrowError::PoolFull);

        // Exact entry fee into the vault.
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.player.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                },
            ),
            pool.entry_fee,
        )?;

        // `init` on the entry PDA guarantees no double entry.
        let entry = &mut ctx.accounts.entry;
        entry.pool = pool.key();
        entry.player = ctx.accounts.player.key();
        entry.amount = pool.entry_fee;
        entry.bump = ctx.bumps.entry;

        pool.player_count = pool.player_count.checked_add(1).ok_or(EscrowError::Overflow)?;

        emit!(PlayerDeposited {
            pool: pool.key(),
            player: entry.player,
            amount: entry.amount,
            player_count: pool.player_count,
        });
        Ok(())
    }

    pub fn settle(ctx: Context<Settle>, winner: Pubkey) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        require!(pool.status == PoolStatus::Open, EscrowError::PoolNotOpen);
        let now = Clock::get()?.unix_timestamp;
        require!(now >= pool.deadline, EscrowError::DeadlineNotReached);
        require_keys_eq!(ctx.accounts.winner.key(), winner, EscrowError::WinnerMismatch);
        // winner_entry PDA (seeded by winner) existing proves winner deposited.
        require_keys_eq!(ctx.accounts.winner_entry.player, winner, EscrowError::WinnerNotDepositor);

        let total = ctx.accounts.vault.lamports();
        let fee = (total as u128)
            .checked_mul(pool.fee_bps as u128)
            .ok_or(EscrowError::Overflow)?
            / 10_000u128;
        let fee = fee as u64;
        let prize = total.checked_sub(fee).ok_or(EscrowError::Overflow)?;

        let pool_key = pool.key();
        let seeds: &[&[u8]] = &[VAULT_SEED, pool_key.as_ref(), &[pool.vault_bump]];
        let signer = &[seeds];
        let sp = ctx.accounts.system_program.to_account_info();
        if fee > 0 {
            system_program::transfer(
                CpiContext::new_with_signer(
                    sp.clone(),
                    Transfer {
                        from: ctx.accounts.vault.to_account_info(),
                        to: ctx.accounts.organizer.to_account_info(),
                    },
                    signer,
                ),
                fee,
            )?;
        }
        if prize > 0 {
            system_program::transfer(
                CpiContext::new_with_signer(
                    sp,
                    Transfer {
                        from: ctx.accounts.vault.to_account_info(),
                        to: ctx.accounts.winner.to_account_info(),
                    },
                    signer,
                ),
                prize,
            )?;
        }

        pool.status = PoolStatus::Settled;
        pool.winner = winner;
        emit!(PoolSettled { pool: pool_key, winner, prize, fee });
        Ok(())
    }

    pub fn cancel(ctx: Context<Cancel>) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        require!(pool.status == PoolStatus::Open, EscrowError::PoolNotOpen);
        pool.status = PoolStatus::Cancelled;
        emit!(PoolCancelled { pool: pool.key(), player_count: pool.player_count });
        Ok(())
    }

    /// Player reclaims their deposit from a cancelled pool. Entry account is
    /// closed (rent back to player), which also prevents double refunds.
    pub fn refund(ctx: Context<Refund>) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        require!(pool.status == PoolStatus::Cancelled, EscrowError::PoolNotCancelled);
        let amount = ctx.accounts.entry.amount;
        let pool_key = pool.key();
        let seeds: &[&[u8]] = &[VAULT_SEED, pool_key.as_ref(), &[pool.vault_bump]];
        system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.player.to_account_info(),
                },
                &[seeds],
            ),
            amount,
        )?;
        pool.refunded_count = pool.refunded_count.checked_add(1).ok_or(EscrowError::Overflow)?;
        emit!(PlayerRefunded { pool: pool_key, player: ctx.accounts.player.key(), amount });
        Ok(())
    }

    /// Organizer closes the pool account (rent back) once it is settled, or
    /// cancelled with every player refunded.
    pub fn close_pool(ctx: Context<ClosePool>) -> Result<()> {
        let pool = &ctx.accounts.pool;
        match pool.status {
            PoolStatus::Settled => {}
            PoolStatus::Cancelled => require!(
                pool.refunded_count == pool.player_count,
                EscrowError::RefundsOutstanding
            ),
            PoolStatus::Open => return err!(EscrowError::PoolStillOpen),
        }
        emit!(PoolClosed { pool: pool.key() });
        Ok(())
    }

    /// Player closes their entry account after the pool was settled (or after
    /// the pool account itself has been closed) to reclaim rent.
    pub fn close_entry(ctx: Context<CloseEntry>) -> Result<()> {
        let pool_info = &ctx.accounts.pool;
        if pool_info.owner == &crate::ID && pool_info.data_len() > 0 {
            let data = pool_info.try_borrow_data()?;
            let pool = Pool::try_deserialize(&mut &data[..])?;
            require!(pool.status == PoolStatus::Settled, EscrowError::PoolNotSettled);
        }
        // else: pool account already closed => pool finished.
        emit!(EntryClosed { pool: pool_info.key(), player: ctx.accounts.player.key() });
        Ok(())
    }
}

// ---------------- Accounts ----------------

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct CreatePool<'info> {
    #[account(mut)]
    pub organizer: Signer<'info>,
    #[account(
        init, payer = organizer, space = 8 + Pool::INIT_SPACE,
        seeds = [POOL_SEED, organizer.key().as_ref(), &pool_id.to_le_bytes()], bump
    )]
    pub pool: Account<'info, Pool>,
    /// CHECK: lamports-only PDA vault owned by the system program.
    #[account(seeds = [VAULT_SEED, pool.key().as_ref()], bump)]
    pub vault: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut)]
    pub player: Signer<'info>,
    #[account(
        mut,
        seeds = [POOL_SEED, pool.organizer.as_ref(), &pool.pool_id.to_le_bytes()], bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
    #[account(mut, seeds = [VAULT_SEED, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    #[account(
        init, payer = player, space = 8 + PlayerEntry::INIT_SPACE,
        seeds = [ENTRY_SEED, pool.key().as_ref(), player.key().as_ref()], bump
    )]
    pub entry: Account<'info, PlayerEntry>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(mut)]
    pub organizer: Signer<'info>,
    #[account(
        mut, has_one = organizer @ EscrowError::Unauthorized,
        seeds = [POOL_SEED, organizer.key().as_ref(), &pool.pool_id.to_le_bytes()], bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
    #[account(mut, seeds = [VAULT_SEED, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    #[account(
        seeds = [ENTRY_SEED, pool.key().as_ref(), winner.key().as_ref()], bump = winner_entry.bump,
        constraint = winner_entry.pool == pool.key() @ EscrowError::WinnerNotDepositor
    )]
    pub winner_entry: Account<'info, PlayerEntry>,
    #[account(mut)]
    pub winner: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Cancel<'info> {
    pub organizer: Signer<'info>,
    #[account(
        mut, has_one = organizer @ EscrowError::Unauthorized,
        seeds = [POOL_SEED, organizer.key().as_ref(), &pool.pool_id.to_le_bytes()], bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
}

#[derive(Accounts)]
pub struct Refund<'info> {
    #[account(mut)]
    pub player: Signer<'info>,
    #[account(
        mut,
        seeds = [POOL_SEED, pool.organizer.as_ref(), &pool.pool_id.to_le_bytes()], bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
    #[account(mut, seeds = [VAULT_SEED, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    #[account(
        mut, close = player,
        seeds = [ENTRY_SEED, pool.key().as_ref(), player.key().as_ref()], bump = entry.bump,
        has_one = player @ EscrowError::Unauthorized,
        has_one = pool @ EscrowError::Unauthorized
    )]
    pub entry: Account<'info, PlayerEntry>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClosePool<'info> {
    #[account(mut)]
    pub organizer: Signer<'info>,
    #[account(
        mut, close = organizer, has_one = organizer @ EscrowError::Unauthorized,
        seeds = [POOL_SEED, organizer.key().as_ref(), &pool.pool_id.to_le_bytes()], bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
}

#[derive(Accounts)]
pub struct CloseEntry<'info> {
    #[account(mut)]
    pub player: Signer<'info>,
    /// CHECK: may be a live Pool or an already-closed account; validated in
    /// handler and via entry.pool / entry PDA seeds.
    pub pool: UncheckedAccount<'info>,
    #[account(
        mut, close = player,
        seeds = [ENTRY_SEED, pool.key().as_ref(), player.key().as_ref()], bump = entry.bump,
        has_one = player @ EscrowError::Unauthorized,
        has_one = pool @ EscrowError::Unauthorized
    )]
    pub entry: Account<'info, PlayerEntry>,
}

// ---------------- State ----------------

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum PoolStatus {
    Open,
    Settled,
    Cancelled,
}

#[account]
#[derive(InitSpace)]
pub struct Pool {
    pub organizer: Pubkey,
    pub pool_id: u64,
    pub entry_fee: u64,
    pub deadline: i64,
    pub max_players: u32,
    pub player_count: u32,
    pub refunded_count: u32,
    pub fee_bps: u16,
    pub status: PoolStatus,
    pub winner: Pubkey,
    pub bump: u8,
    pub vault_bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct PlayerEntry {
    pub pool: Pubkey,
    pub player: Pubkey,
    pub amount: u64,
    pub bump: u8,
}

// ---------------- Events ----------------

#[event]
pub struct PoolCreated {
    pub pool: Pubkey,
    pub organizer: Pubkey,
    pub pool_id: u64,
    pub entry_fee: u64,
    pub deadline: i64,
    pub max_players: u32,
    pub fee_bps: u16,
}
#[event]
pub struct PlayerDeposited { pub pool: Pubkey, pub player: Pubkey, pub amount: u64, pub player_count: u32 }
#[event]
pub struct PoolSettled { pub pool: Pubkey, pub winner: Pubkey, pub prize: u64, pub fee: u64 }
#[event]
pub struct PoolCancelled { pub pool: Pubkey, pub player_count: u32 }
#[event]
pub struct PlayerRefunded { pub pool: Pubkey, pub player: Pubkey, pub amount: u64 }
#[event]
pub struct PoolClosed { pub pool: Pubkey }
#[event]
pub struct EntryClosed { pub pool: Pubkey, pub player: Pubkey }

// ---------------- Errors ----------------

#[error_code]
pub enum EscrowError {
    #[msg("Entry fee must be at least the rent-exempt minimum (890,880 lamports)")]
    InvalidEntryFee,
    #[msg("Max players must be at least 1")]
    InvalidMaxPlayers,
    #[msg("Organizer fee exceeds 10% (1000 bps)")]
    FeeTooHigh,
    #[msg("Deadline must be in the future")]
    DeadlineInPast,
    #[msg("Pool is not open")]
    PoolNotOpen,
    #[msg("Deposit deadline has passed")]
    DeadlinePassed,
    #[msg("Deadline not reached yet")]
    DeadlineNotReached,
    #[msg("Pool is full")]
    PoolFull,
    #[msg("Winner account does not match winner argument")]
    WinnerMismatch,
    #[msg("Winner did not deposit into this pool")]
    WinnerNotDepositor,
    #[msg("Pool is not cancelled")]
    PoolNotCancelled,
    #[msg("Pool is not settled")]
    PoolNotSettled,
    #[msg("Pool is still open")]
    PoolStillOpen,
    #[msg("Some players have not been refunded yet")]
    RefundsOutstanding,
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Arithmetic overflow")]
    Overflow,
}
