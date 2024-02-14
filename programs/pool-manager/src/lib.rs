use anchor_lang::prelude::*;
use anchor_spl::token::{Token, Mint, MintTo, Burn, TokenAccount, Transfer, mint_to, burn, transfer};
use locker_manager::{CreateLocker, RewardKeeper, Locker, cpi as LockerManagerCPI};
use std::collections::BTreeMap;
use std::convert::Into;

declare_id!("362QKmGMs4ZGiSfuQCXCEAcW6gFExy9XmZM2Cki89rWw");

#[program]
mod pool_manager {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, _pool_manager_nonce: u64) -> Result<()> {
        ctx.accounts.pool_manager_info.authority = *ctx.accounts.authority.key;
        ctx.accounts.pool_manager_info.locker_manager_program = *ctx.accounts.locker_manager_program.key;
        ctx.accounts.pool_manager_info.reward_keeper_program = *ctx.accounts.reward_keeper_program.key;

        Ok(())
    }

    pub fn set_locker_manager(
        ctx: Context<Auth>,
        _pool_manager_nonce: u64,
        locker_manager_program: Pubkey,
    ) -> Result<()> {
        ctx.accounts.pool_manager_info.locker_manager_program = locker_manager_program;

        Ok(())
    }

    pub fn set_authority(ctx: Context<Auth>, _pool_manager_nonce: u64, new_authority: Pubkey) -> Result<()> {
        ctx.accounts.pool_manager_info.authority = new_authority;

        Ok(())
    }

    pub fn create_pool(
        ctx: Context<CreatePool>,
        mint: Pubkey,
        authority: Pubkey,
        nonce: u8,
        withdrawal_timelock: i64,
        stake_rate: u64,
        reward_q_len: u32,
    ) -> Result<()> {
        let pool = &mut ctx.accounts.pool;

        pool.authority = authority;
        pool.nonce = nonce;
        pool.mint = mint;
        pool.pool_mint = *ctx.accounts.pool_mint.to_account_info().key;
        pool.stake_rate = stake_rate;
        pool.reward_event_q = *ctx.accounts.reward_event_q.to_account_info().key;
        pool.withdrawal_timelock = withdrawal_timelock;

        let reward_q = &mut ctx.accounts.reward_event_q;
        reward_q
            .events
            .resize(reward_q_len as usize, Default::default());

        Ok(())
    }

    pub fn update_pool(
        ctx: Context<UpdatePool>,
        new_authority: Option<Pubkey>,
        withdrawal_timelock: Option<i64>,
    ) -> Result<()> {
        let pool = &mut ctx.accounts.pool;

        if let Some(new_authority) = new_authority {
            pool.authority = new_authority;
        }

        if let Some(withdrawal_timelock) = withdrawal_timelock {
            pool.withdrawal_timelock = withdrawal_timelock;
        }

        Ok(())
    }

    pub fn create_staker(ctx: Context<CreateStaker>, nonce: u8) -> Result<()> {
        let staker = &mut ctx.accounts.staker;
        staker.pool = *ctx.accounts.pool.to_account_info().key;
        staker.beneficiary = *ctx.accounts.beneficiary.key;
        staker.nonce = nonce;

        Ok(())
    }

    pub fn update_staker(ctx: Context<UpdateStaker>, metadata: Option<Pubkey>) -> Result<()> {
        let staker = &mut ctx.accounts.staker;
        if let Some(m) = metadata {
            staker.metadata = m;
        }

        Ok(())
    }

    pub fn update_staker_vault(ctx: Context<UpdateStakerVault>, nonce: u8) -> Result<()> {
        let staker = &mut ctx.accounts.staker;
        staker.staker_vault = (&ctx.accounts.staker_vault).into();

        Ok(())
    }

    pub fn update_staker_vault_locked(
        ctx: Context<UpdateStakerVaultLocked>,
        nonce: u8,
    ) -> Result<()> {
        let staker = &mut ctx.accounts.staker;
        staker.staker_vault_locked = (&ctx.accounts.staker_vault_locked).into();

        Ok(())
    }

    pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        transfer(ctx.accounts.into(), amount).map_err(Into::into)
    }

    pub fn stake(ctx: Context<Stake>, pool_token_amount: u64) -> Result<()> {
        // Transfer tokens into the stake vault.
        {
            let seeds = &[
                ctx.accounts.pool.to_account_info().key.as_ref(),
                ctx.accounts.staker.to_account_info().key.as_ref(),
                &[ctx.accounts.staker.nonce],
            ];
            let signer_seeds = &[&seeds[..]];
            let cpi_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info().clone(),
                Transfer {
                    from: ctx.accounts.staker_vault.vault.to_account_info(),
                    to: ctx.accounts.staker_vault.vault_staked.to_account_info(),
                    authority: ctx.accounts.staker_vault_authority.to_account_info(),
                },
                signer_seeds,
            );
            // Convert from stake-token units to mint-token units.
            let token_amount = pool_token_amount
                .checked_mul(ctx.accounts.pool.stake_rate)
                .unwrap();
            transfer(cpi_ctx, token_amount)?;
        }

        // Mint pool tokens to the staker.
        {
            let seeds = &[
                ctx.accounts.pool.to_account_info().key.as_ref(),
                &[ctx.accounts.pool.nonce],
            ];
            let signer_seeds = &[&seeds[..]];

            let cpi_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info().clone(),
                MintTo {
                    mint: ctx.accounts.pool_mint.to_account_info(),
                    to: ctx.accounts.staker_vault.vault_pool_token.to_account_info(),
                    authority: ctx.accounts.pool_mint_authority.to_account_info(),
                },
                signer_seeds,
            );
            mint_to(cpi_ctx, pool_token_amount)?;
        }

        // Update stake timestamp.
        let staker = &mut ctx.accounts.staker;
        staker.last_stake_ts = ctx.accounts.clock.unix_timestamp;

        Ok(())
    }

    pub fn start_unstake(ctx: Context<StartUnstake>, pool_token_amount: u64, locked: bool) -> Result<()> {
        // Program signer.
        let seeds = &[
            ctx.accounts.pool.to_account_info().key.as_ref(),
            ctx.accounts.staker.to_account_info().key.as_ref(),
            &[ctx.accounts.staker.nonce],
        ];
        let signer_seeds = &[&seeds[..]];

        // Burn pool tokens.
        {
            let cpi_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info().clone(),
                Burn {
                    mint: ctx.accounts.pool_mint.to_account_info(),
                    from: ctx.accounts.staker_vault.vault_pool_token.to_account_info(),
                    authority: ctx.accounts.staker_vault_authority.to_account_info(),
                },
                signer_seeds,
            );
            burn(cpi_ctx, pool_token_amount)?;
        }

        // Convert from stake-token units to mint-token units.
        let token_amount = pool_token_amount
            .checked_mul(ctx.accounts.pool.stake_rate)
            .unwrap();

        // Transfer tokens from the stake to pending vault.
        {
            let cpi_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info().clone(),
                Transfer {
                    from: ctx.accounts.staker_vault.vault_staked.to_account_info(),
                    to: ctx.accounts.staker_vault.vault_pending_withdrawal.to_account_info(),
                    authority: ctx.accounts.staker_vault_authority.to_account_info(),
                },
                signer_seeds,
            );
            transfer(cpi_ctx, token_amount)?;
        }

        // Print receipt.
        let pending_withdrawal = &mut ctx.accounts.pending_withdrawal;
        pending_withdrawal.burned = false;
        pending_withdrawal.staker = *ctx.accounts.staker.to_account_info().key;
        pending_withdrawal.start_ts = ctx.accounts.clock.unix_timestamp;
        pending_withdrawal.end_ts =
            ctx.accounts.clock.unix_timestamp + ctx.accounts.pool.withdrawal_timelock;
        pending_withdrawal.amount = token_amount;
        pending_withdrawal.pool_mint = ctx.accounts.pool.pool_mint;
        pending_withdrawal.pool = *ctx.accounts.pool.to_account_info().key;
        pending_withdrawal.locked = locked;

        // Update stake timestamp.
        let staker = &mut ctx.accounts.staker;
        staker.last_stake_ts = ctx.accounts.clock.unix_timestamp;

        Ok(())
    }

    pub fn end_unstake(ctx: Context<EndUnstake>) -> Result<()> {
        // Select which balance set this affects.
        let staker_vault = {
            if ctx.accounts.pending_withdrawal.locked {
                &ctx.accounts.staker.staker_vault_locked
            } else {
                &ctx.accounts.staker.staker_vault
            }
        };

        // Transfer tokens between vaults.
        {
            let seeds = &[
                ctx.accounts.pool.to_account_info().key.as_ref(),
                ctx.accounts.staker.to_account_info().key.as_ref(),
                &[ctx.accounts.staker.nonce],
            ];
            let signer = &[&seeds[..]];
            let cpi_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info().clone(),
                Transfer {
                    from: ctx.accounts.vault_pending_withdrawal.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.staker_vault_authority.clone(),
                },
                signer,
            );
            transfer(cpi_ctx, ctx.accounts.pending_withdrawal.amount)?;
        }

        // Burn the pending withdrawal receipt.
        let pending_withdrawal = &mut ctx.accounts.pending_withdrawal;
        pending_withdrawal.burned = true;

        Ok(())
    }

    pub fn withdraw(ctx: Context<Withdraw>, amount: u64) -> Result<()> {
        let seeds = &[
            ctx.accounts.pool.to_account_info().key.as_ref(),
            ctx.accounts.staker.to_account_info().key.as_ref(),
            &[ctx.accounts.staker.nonce],
        ];
        let signer = &[&seeds[..]];
        let cpi_accounts = Transfer {
            from: ctx.accounts.vault.to_account_info(),
            to: ctx.accounts.depositor.to_account_info(),
            authority: ctx.accounts.staker_vault_authority.clone(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info().clone();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);

        transfer(cpi_ctx, amount).map_err(Into::into)
    }

    pub fn drop_reward(
        ctx: Context<DropReward>,
        kind: RewarderKind,
        total: u64,
        expiry_ts: i64,
        expiry_receiver: Pubkey,
        nonce: u8,
    ) -> Result<()> {
        if let RewarderKind::Locked {
            start_ts,
            end_ts,
            period_count,
        } = kind {};

        // Transfer funds into the rewarder's vault.
        transfer(ctx.accounts.into(), total)?;

        // Add the event to the reward queue.
        let reward_q = &mut ctx.accounts.reward_event_q;
        let cursor = reward_q.append(RewardEvent {
            rewarder: *ctx.accounts.rewarder.to_account_info().key,
            ts: ctx.accounts.clock.unix_timestamp,
            locked: kind != RewarderKind::Unlocked,
        })?;

        // Initialize the rewarder.
        let rewarder = &mut ctx.accounts.rewarder;
        rewarder.pool = *ctx.accounts.pool.to_account_info().key;
        rewarder.vault = *ctx.accounts.rewarder_vault.to_account_info().key;
        rewarder.mint = ctx.accounts.rewarder_vault.mint;
        rewarder.nonce = nonce;
        rewarder.pool_token_supply = ctx.accounts.pool_mint.supply;
        rewarder.reward_event_q_cursor = cursor;
        rewarder.start_ts = ctx.accounts.clock.unix_timestamp;
        rewarder.expiry_ts = expiry_ts;
        rewarder.expiry_receiver = expiry_receiver;
        rewarder.from = *ctx.accounts.depositor_authority.key;
        rewarder.total = total;
        rewarder.expired = false;
        rewarder.kind = kind;

        Ok(())
    }

    pub fn claim_reward(ctx: Context<ClaimReward>) -> Result<()> {
        // Reward distribution.
        let pool_token_total =
            ctx.accounts.common.staker_vault_pool_token.amount + ctx.accounts.common.staker_vault_locked_pool_token.amount;
        let reward_amount = pool_token_total
            .checked_mul(ctx.accounts.common.rewarder.total)
            .unwrap()
            .checked_div(ctx.accounts.common.rewarder.pool_token_supply)
            .unwrap();
        assert!(reward_amount > 0);

        // Send reward to the given token account.
        let seeds = &[
            ctx.accounts.common.pool.to_account_info().key.as_ref(),
            ctx.accounts.common.rewarder.to_account_info().key.as_ref(),
            &[ctx.accounts.common.rewarder.nonce],
        ];
        let signer = &[&seeds[..]];
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.common.token_program.to_account_info().clone(),
            Transfer {
                from: ctx.accounts.common.vault.to_account_info(),
                to: ctx.accounts.to.to_account_info(),
                authority: ctx.accounts.common.rewarder_vault_authority.to_account_info(),
            },
            signer,
        );
        transfer(cpi_ctx, reward_amount)?;

        // Update staker as having processed the reward.
        let staker = &mut ctx.accounts.common.staker;
        staker.rewards_cursor = ctx.accounts.common.rewarder.reward_event_q_cursor + 1;

        Ok(())
    }

    pub fn claim_reward_to_locker<'a, 'b, 'c, 'info>(
        ctx: Context<'a, 'b, 'c, 'info, ClaimRewardToLocker<'info>>,
        _pool_manager_nonce: u64,
        nonce: u8,
    ) -> Result<()> {
        let (start_ts, end_ts, period_count) = match ctx.accounts.common.rewarder.kind {
            RewarderKind::Unlocked => return Err(ProgramError::InvalidAccountData.into()),
            RewarderKind::Locked {
                start_ts,
                end_ts,
                period_count,
            } => (start_ts, end_ts, period_count),
        };

        // Reward distribution.
        let pool_token_total =
            ctx.accounts.common.staker_vault_pool_token.amount + ctx.accounts.common.staker_vault_locked_pool_token.amount;
        let reward_amount = pool_token_total
            .checked_mul(ctx.accounts.common.rewarder.total)
            .unwrap()
            .checked_div(ctx.accounts.common.rewarder.pool_token_supply)
            .unwrap();
        assert!(reward_amount > 0);

        // Specify the locker account's reward_keeper, so that unlocks can only
        // execute once completely unstaked.
        let reward_keeper = Some(RewardKeeper {
            // program: *ctx.program_id,
            program: ctx.accounts.reward_keeper_program.key(),
            metadata: *ctx.accounts.common.staker.to_account_info().key,
        });

        // CPI: Createer account for the staker's beneficiary.
        let seeds = &[
            ctx.accounts.common.pool.to_account_info().key.as_ref(),
            ctx.accounts.common.rewarder.to_account_info().key.as_ref(),
            &[ctx.accounts.common.rewarder.nonce],
        ];
        let signer = &[&seeds[..]];

        let mut depositor_authority = ctx.accounts.common.rewarder_vault_authority.clone();
        depositor_authority.is_signer = true;

        let mut new_remaining_accounts = &[
            ctx.remaining_accounts[0].clone(),
            ctx.remaining_accounts[1].clone(),
            ctx.remaining_accounts[2].clone(),
            depositor_authority.clone(),
            ctx.remaining_accounts[4].clone(),
            ctx.remaining_accounts[5].clone(),
            ctx.remaining_accounts[6].clone(),
        ][..];

        let cpi_program = ctx.accounts.locker_manager_program.clone();
        let mut bumps = BTreeMap::new();
        let cpi_accounts = {
            let accs = CreateLocker::try_accounts(
                ctx.accounts.locker_manager_program.key,
                &mut new_remaining_accounts,
                &[],
                &mut bumps,
            )?;
            LockerManagerCPI::accounts::CreateLocker {
                locker: accs.locker.to_account_info(),
                vault: accs.vault.to_account_info(),
                depositor: accs.depositor.to_account_info(),
                depositor_authority: accs.depositor_authority.to_account_info(),
                token_program: accs.token_program.to_account_info(),
                clock: accs.clock.to_account_info(),
                rent: accs.rent.to_account_info(),
            }
        };
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        LockerManagerCPI::create_locker(
            cpi_ctx,
            ctx.accounts.common.staker.beneficiary,
            reward_amount,
            nonce,
            start_ts,
            end_ts,
            period_count,
            reward_keeper,
        )?;

        // Make sure this reward can't be processed more than once.
        let staker = &mut ctx.accounts.common.staker;
        staker.rewards_cursor = ctx.accounts.common.rewarder.reward_event_q_cursor + 1;

        Ok(())
    }

    pub fn expire_reward(ctx: Context<ExpireReward>) -> Result<()> {
        // Send all remaining funds to the expiry receiver's token.
        let seeds = &[
            ctx.accounts.pool.to_account_info().key.as_ref(),
            ctx.accounts.rewarder.to_account_info().key.as_ref(),
            &[ctx.accounts.rewarder.nonce],
        ];
        let signer = &[&seeds[..]];
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info().clone(),
            Transfer {
                to: ctx.accounts.expiry_receiver_token.to_account_info(),
                from: ctx.accounts.vault.to_account_info(),
                authority: ctx.accounts.rewarder_vault_authority.to_account_info(),
            },
            signer,
        );
        transfer(cpi_ctx, ctx.accounts.vault.amount)?;

        // Burn the rewarder.
        let rewarder = &mut ctx.accounts.rewarder;
        rewarder.expired = true;

        Ok(())
    }
}

#[account]
pub struct PoolManagerInfo {
    pub authority: Pubkey,
    pub locker_manager_program: Pubkey,
    pub reward_keeper_program: Pubkey,
}

#[derive(Accounts)]
pub struct CreatePool<'info> {
    #[account(zero)]
    pool: Box<Account<'info, Pool>>,
    #[account(zero)]
    reward_event_q: Box<Account<'info, RewardQueue>>,
    #[account(constraint = pool_mint.decimals == 0)]
    pool_mint: Account<'info, Mint>,
    rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct UpdatePool<'info> {
    #[account(mut, has_one = authority)]
    pool: Box<Account<'info, Pool>>,
    authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct CreateStaker<'info> {
    pool: Box<Account<'info, Pool>>,
    #[account(zero)]
    staker: Box<Account<'info, Staker>>,
    beneficiary: Signer<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    staker_vault_authority: AccountInfo<'info>,
    token_program: Program<'info, Token>,
    rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct UpdateStakerVault<'info> {
    pool: Box<Account<'info, Pool>>,
    #[account(mut)]
    staker: Box<Account<'info, Staker>>,
    #[account(
        constraint = &staker_vault.vault_pool_token.owner == staker_vault_authority.key,
        constraint = staker_vault.vault_pool_token.mint == pool.pool_mint,
        constraint = staker_vault.vault.mint == pool.mint
    )]
    staker_vault: StakerVaultAccounts<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    staker_vault_authority: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct UpdateStakerVaultLocked<'info> {
    pool: Box<Account<'info, Pool>>,
    #[account(mut)]
    staker: Box<Account<'info, Staker>>,
    #[account(
        constraint = &staker_vault_locked.vault_pool_token.owner == staker_vault_authority.key,
        constraint = staker_vault_locked.vault_pool_token.mint == pool.pool_mint,
        constraint = staker_vault_locked.vault.mint == pool.mint
    )]
    staker_vault_locked: StakerVaultAccounts<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    staker_vault_authority: AccountInfo<'info>,
}

#[derive(Accounts, Clone)]
pub struct StakerVaultAccounts<'info> {
    #[account(mut, constraint = vault.owner == vault_pool_token.owner)]
    vault: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = vault_staked.owner == vault_pool_token.owner,
        constraint = vault_staked.mint == vault.mint
    )]
    vault_staked: Box<Account<'info, TokenAccount>>,
    #[account(mut,
        constraint = vault_pending_withdrawal.owner == vault_pool_token.owner,
        constraint = vault_pending_withdrawal.mint == vault.mint
    )]
    vault_pending_withdrawal: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    vault_pool_token: Box<Account<'info, TokenAccount>>,
}

#[derive(Accounts)]
#[instruction(pool_manager_nonce: u64)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    pub locker_manager_program: AccountInfo<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    pub reward_keeper_program: AccountInfo<'info>,
    #[account(
        init,
        seeds = [b"pool-manager".as_ref(), &pool_manager_nonce.to_le_bytes()],
        bump,
        payer = authority,
        space = 8 + 32 + 32 + 32
    )]
    pub pool_manager_info: Box<Account<'info, PoolManagerInfo>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(pool_manager_nonce: u64)]
pub struct Auth<'info> {
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [b"pool-manager".as_ref(), &pool_manager_nonce.to_le_bytes()],
        bump
    )]
    pub pool_manager_info: Box<Account<'info, PoolManagerInfo>>,
}

#[derive(Accounts)]
pub struct UpdateStaker<'info> {
    #[account(mut, has_one = beneficiary)]
    staker: Box<Account<'info, Staker>>,
    beneficiary: Signer<'info>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(has_one = beneficiary)]
    staker: Box<Account<'info, Staker>>,
    beneficiary: Signer<'info>,
    #[account(mut, constraint = vault.to_account_info().key == &staker.staker_vault.vault)]
    vault: Account<'info, TokenAccount>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    depositor: AccountInfo<'info>,
    #[account(constraint = depositor_authority.key == &staker.beneficiary)]
    depositor_authority: Signer<'info>,
    token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(has_one = pool_mint, has_one = reward_event_q)]
    pool: Box<Account<'info, Pool>>,
    reward_event_q: Box<Account<'info, RewardQueue>>,
    #[account(mut)]
    pool_mint: Box<Account<'info, Mint>>,
    #[account(mut, has_one = beneficiary, has_one = pool)]
    staker: Box<Account<'info, Staker>>,
    beneficiary: Signer<'info>,
    #[account(constraint = StakerVault::from(&staker_vault) == staker.staker_vault)]
    staker_vault: StakerVaultAccounts<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(
        seeds = [
            pool.to_account_info().key.as_ref(),
            staker.to_account_info().key.as_ref(),
        ],
        bump = staker.nonce
    )]
    staker_vault_authority: AccountInfo<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(seeds = [pool.to_account_info().key.as_ref()], bump = pool.nonce)]
    pool_mint_authority: AccountInfo<'info>,
    clock: Sysvar<'info, Clock>,
    token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct StartUnstake<'info> {
    #[account(has_one = reward_event_q, has_one = pool_mint)]
    pool: Box<Account<'info, Pool>>,
    reward_event_q: Box<Account<'info, RewardQueue>>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    pool_mint: AccountInfo<'info>,
    #[account(zero)]
    pending_withdrawal: Box<Account<'info, PendingWithdrawal>>,
    #[account(has_one = beneficiary, has_one = pool)]
    staker: Box<Account<'info, Staker>>,
    beneficiary: Signer<'info>,
    #[account(constraint = StakerVault::from(&staker_vault) == staker.staker_vault)]
    staker_vault: StakerVaultAccounts<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(
        seeds = [
            pool.to_account_info().key.as_ref(),
            staker.to_account_info().key.as_ref(),
        ],
        bump = staker.nonce
    )]
    staker_vault_authority: AccountInfo<'info>,
    token_program: Program<'info, Token>,
    clock: Sysvar<'info, Clock>,
    rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct EndUnstake<'info> {
    pool: Box<Account<'info, Pool>>,
    #[account(has_one = pool, has_one = beneficiary)]
    staker: Box<Account<'info, Staker>>,
    beneficiary: Signer<'info>,
    #[account(mut, has_one = pool, has_one = staker, constraint = !pending_withdrawal.burned)]
    pending_withdrawal: Box<Account<'info, PendingWithdrawal>>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    vault: AccountInfo<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    vault_pending_withdrawal: AccountInfo<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(
        seeds = [
            pool.to_account_info().key.as_ref(),
            staker.to_account_info().key.as_ref(),
        ],
        bump = staker.nonce
    )]
    staker_vault_authority: AccountInfo<'info>,
    clock: Sysvar<'info, Clock>,
    token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    pool: Box<Account<'info, Pool>>,
    #[account(has_one = pool, has_one = beneficiary)]
    staker: Box<Account<'info, Staker>>,
    beneficiary: Signer<'info>,
    #[account(mut, constraint = vault.to_account_info().key == &staker.staker_vault.vault)]
    vault: Account<'info, TokenAccount>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(
        seeds = [
            pool.to_account_info().key.as_ref(),
            staker.to_account_info().key.as_ref(),
        ],
        bump = staker.nonce
    )]
    staker_vault_authority: AccountInfo<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    depositor: AccountInfo<'info>,
    token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct DropReward<'info> {
    #[account(has_one = reward_event_q, has_one = pool_mint)]
    pool: Box<Account<'info, Pool>>,
    #[account(mut)]
    reward_event_q: Box<Account<'info, RewardQueue>>,
    pool_mint: Account<'info, Mint>,
    #[account(zero)]
    rewarder: Box<Account<'info, Rewarder>>,
    #[account(mut)]
    rewarder_vault: Account<'info, TokenAccount>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    depositor: AccountInfo<'info>,
    depositor_authority: Signer<'info>,
    token_program: Program<'info, Token>,
    clock: Sysvar<'info, Clock>,
    rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct ClaimReward<'info> {
    common: ClaimRewardCommon<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    to: AccountInfo<'info>,
}

#[derive(Accounts)]
#[instruction(pool_manager_nonce: u64)]
pub struct ClaimRewardToLocker<'info> {
    common: ClaimRewardCommon<'info>,
    #[account(seeds = [b"pool-manager".as_ref(), &pool_manager_nonce.to_le_bytes()], bump)]
    pool_manager_info: Box<Account<'info, PoolManagerInfo>>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(constraint = locker_manager_program.key == &pool_manager_info.locker_manager_program)]
    locker_manager_program: AccountInfo<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(constraint = reward_keeper_program.key == &pool_manager_info.reward_keeper_program)]
    reward_keeper_program: AccountInfo<'info>,
}

// Accounts common to both claim reward locked/unlocked instructions.
#[derive(Accounts)]
pub struct ClaimRewardCommon<'info> {
    pool: Box<Account<'info, Pool>>,
    #[account(mut, has_one = pool, has_one = beneficiary)]
    staker: Box<Account<'info, Staker>>,
    beneficiary: Signer<'info>,
    #[account(constraint = staker_vault_pool_token.key() == staker.staker_vault.vault_pool_token)]
    staker_vault_pool_token: Account<'info, TokenAccount>,
    #[account(constraint = staker_vault_locked_pool_token.key() == staker.staker_vault_locked.vault_pool_token)]
    staker_vault_locked_pool_token: Account<'info, TokenAccount>,
    #[account(has_one = pool, has_one = vault)]
    rewarder: Box<Account<'info, Rewarder>>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    vault: AccountInfo<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(
        seeds = [
            pool.to_account_info().key.as_ref(),
            rewarder.to_account_info().key.as_ref(),
        ],
        bump = rewarder.nonce
    )]
    rewarder_vault_authority: AccountInfo<'info>,
    token_program: Program<'info, Token>,
    clock: Sysvar<'info, Clock>,
}

#[derive(Accounts)]
pub struct ExpireReward<'info> {
    pool: Box<Account<'info, Pool>>,
    #[account(mut, has_one = pool, has_one = vault, has_one = expiry_receiver)]
    rewarder: Box<Account<'info, Rewarder>>,
    #[account(mut)]
    vault: Account<'info, TokenAccount>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(
        seeds = [
            pool.to_account_info().key.as_ref(),
            rewarder.to_account_info().key.as_ref(),
        ],
        bump = rewarder.nonce
    )]
    rewarder_vault_authority: AccountInfo<'info>,
    expiry_receiver: Signer<'info>,
    /// CHECK: This is not dangerous because we don't read or write from this account
    #[account(mut)]
    expiry_receiver_token: AccountInfo<'info>,
    token_program: Program<'info, Token>,
    clock: Sysvar<'info, Clock>,
}

#[account]
pub struct Pool {
    pub authority: Pubkey,
    pub nonce: u8,
    pub withdrawal_timelock: i64,
    pub reward_event_q: Pubkey,
    pub mint: Pubkey,
    pub pool_mint: Pubkey,
    pub stake_rate: u64,
}

#[account]
pub struct Staker {
    pub pool: Pubkey,
    pub beneficiary: Pubkey,
    pub metadata: Pubkey,
    pub staker_vault: StakerVault,
    pub staker_vault_locked: StakerVault,
    pub rewards_cursor: u32,
    pub last_stake_ts: i64,
    pub nonce: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Default, Debug, Clone, PartialEq)]
pub struct StakerVault {
    pub vault: Pubkey,
    pub vault_staked: Pubkey,
    pub vault_pending_withdrawal: Pubkey,
    pub vault_pool_token: Pubkey,
}

#[account]
pub struct PendingWithdrawal {
    pub pool: Pubkey,
    pub staker: Pubkey,
    pub burned: bool,
    pub pool_mint: Pubkey,
    pub start_ts: i64,
    pub end_ts: i64,
    pub amount: u64,
    pub locked: bool,
}

#[account]
pub struct RewardQueue {
    head: u32,
    tail: u32,
    events: Vec<RewardEvent>,
}

impl RewardQueue {
    pub fn append(&mut self, event: RewardEvent) -> Result<u32> {
        let cursor = self.head;

        // Insert into next available slot.
        let h_idx = self.index_of(self.head);
        self.events[h_idx] = event;

        // Update head and tail counters.
        let is_full = self.index_of(self.head + 1) == self.index_of(self.tail);
        if is_full {
            self.tail += 1;
        }
        self.head += 1;

        Ok(cursor)
    }

    pub fn index_of(&self, counter: u32) -> usize {
        counter as usize % self.capacity()
    }

    pub fn capacity(&self) -> usize {
        self.events.len()
    }

    pub fn get(&self, cursor: u32) -> &RewardEvent {
        &self.events[cursor as usize % self.capacity()]
    }

    pub fn head(&self) -> u32 {
        self.head
    }

    pub fn tail(&self) -> u32 {
        self.tail
    }
}

#[derive(Default, Clone, Copy, Debug, AnchorSerialize, AnchorDeserialize)]
pub struct RewardEvent {
    rewarder: Pubkey,
    ts: i64,
    locked: bool,
}

#[account]
pub struct Rewarder {
    pub pool: Pubkey,
    pub vault: Pubkey,
    pub mint: Pubkey,
    pub nonce: u8,
    pub pool_token_supply: u64,
    pub reward_event_q_cursor: u32,
    pub start_ts: i64,
    pub expiry_ts: i64,
    pub expiry_receiver: Pubkey,
    pub from: Pubkey,
    pub total: u64,
    pub expired: bool,
    pub kind: RewarderKind,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum RewarderKind {
    Unlocked,
    Locked {
        start_ts: i64,
        end_ts: i64,
        period_count: u64,
    },
}

impl<'a, 'b, 'c, 'info> From<&mut Deposit<'info>> for CpiContext<'a, 'b, 'c, 'info, Transfer<'info>> {
    fn from(accounts: &mut Deposit<'info>) -> CpiContext<'a, 'b, 'c, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: accounts.depositor.clone(),
            to: accounts.vault.to_account_info(),
            authority: accounts.depositor_authority.to_account_info().clone(),
        };
        let cpi_program = accounts.token_program.to_account_info().clone();
        CpiContext::new(cpi_program, cpi_accounts)
    }
}

impl<'a, 'b, 'c, 'info> From<&mut DropReward<'info>> for CpiContext<'a, 'b, 'c, 'info, Transfer<'info>> {
    fn from(accounts: &mut DropReward<'info>) -> CpiContext<'a, 'b, 'c, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: accounts.depositor.clone(),
            to: accounts.rewarder_vault.to_account_info(),
            authority: accounts.depositor_authority.to_account_info().clone(),
        };
        let cpi_program = accounts.token_program.to_account_info().clone();
        CpiContext::new(cpi_program, cpi_accounts)
    }
}

impl<'info> From<&StakerVaultAccounts<'info>> for StakerVault {
    fn from(accs: &StakerVaultAccounts<'info>) -> Self {
        Self {
            vault_pool_token: *accs.vault_pool_token.to_account_info().key,
            vault: *accs.vault.to_account_info().key,
            vault_staked: *accs.vault_staked.to_account_info().key,
            vault_pending_withdrawal: *accs.vault_pending_withdrawal.to_account_info().key,
        }
    }
}
