#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod dynamic_fee;
mod errors;
mod events;
mod fee_decay;
mod flash_loan;
mod math;
mod oracle;
mod reentrancy;
mod storage;

#[cfg(test)]
mod test;

use dynamic_fee::compute_fee_bps;
use errors::PairError;
use events::PairEvents;
use math::MINIMUM_LIQUIDITY;
use soroban_sdk::{contract, contractclient, contractimpl, token::TokenClient, Address, Bytes, Env};
use storage::{
    get_fee_state, get_pair_state, set_fee_state, set_pair_state, set_reentrancy_guard, FeeState,
    ReentrancyGuard,
};

#[contractclient(name = "LpTokenClient")]
pub trait LpTokenInterface {
    fn mint(env: Env, to: Address, amount: i128);
    fn burn(env: Env, from: Address, amount: i128);
    fn total_supply(env: Env) -> i128;
}

#[contract]
pub struct Pair;

#[contractimpl]
impl Pair {
    // ─────────────────────────────────────────
    // Initialize
    // ─────────────────────────────────────────

    pub fn initialize(
        env: Env,
        factory: Address,
        token_a: Address,
        token_b: Address,
        lp_token: Address,
    ) -> Result<(), PairError> {
        // 1. Double-init guard
        if get_pair_state(&env).is_some() {
            return Err(PairError::AlreadyInitialized);
        }

        // 2. Identical-token guard
        if token_a == token_b {
            return Err(PairError::InvalidInput);
        }

        let state = storage::PairStorage {
            factory,
            token_a,
            token_b,
            lp_token,
            reserve_a: 0,
            reserve_b: 0,
            block_timestamp_last: env.ledger().timestamp(),
            price_a_cumulative: 0,
            price_b_cumulative: 0,
            k_last: 0,
        };

        set_pair_state(&env, &state);

        // 4. Initialize FeeState with sane defaults
        let fee_state = FeeState {
            vol_accumulator: 0,
            ema_alpha: 10_000_000_000_000, // 10% of SCALE (1e14)
            baseline_fee_bps: 30,
            min_fee_bps: 10,
            max_fee_bps: 100,
            ramp_up_multiplier: 2,
            cooldown_divisor: 2,
            last_fee_update: 0,
            decay_threshold_blocks: 100,
        };
        set_fee_state(&env, &fee_state);

        // 5. Initialize ReentrancyGuard as unlocked
        set_reentrancy_guard(&env, &ReentrancyGuard { locked: false });

        // 6. Extend instance storage TTL (~7 days at 5s/ledger)
        const TTL_THRESHOLD: u32 = 60_480;
        const TTL_EXTEND_TO: u32 = 120_960;
        env.storage().instance().extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);

        Ok(())
    }

    // ─────────────────────────────────────────
    // Mint
    // ─────────────────────────────────────────

    pub fn mint(env: Env, to: Address) -> Result<i128, PairError> {
        to.require_auth();

        let mut state = get_pair_state(&env).ok_or(PairError::NotInitialized)?;
        let contract = env.current_contract_address();

        let balance_a = TokenClient::new(&env, &state.token_a).balance(&contract);
        let balance_b = TokenClient::new(&env, &state.token_b).balance(&contract);

        let amount_a = balance_a.checked_sub(state.reserve_a).ok_or(PairError::InvalidInput)?;

        let amount_b = balance_b.checked_sub(state.reserve_b).ok_or(PairError::InvalidInput)?;

        let lp_client = LpTokenClient::new(&env, &state.lp_token);
        let total_supply = lp_client.total_supply();

        let liquidity;

        if total_supply == 0 {
            let product = amount_a.checked_mul(amount_b).ok_or(PairError::Overflow)?;

            liquidity = math::sqrt(product)
                .checked_sub(MINIMUM_LIQUIDITY)
                .ok_or(PairError::InsufficientLiquidityMinted)?;

            lp_client.mint(&contract, &MINIMUM_LIQUIDITY);
        } else {
            let liquidity_a = amount_a
                .checked_mul(total_supply)
                .ok_or(PairError::Overflow)?
                .checked_div(state.reserve_a)
                .ok_or(PairError::Overflow)?;

            let liquidity_b = amount_b
                .checked_mul(total_supply)
                .ok_or(PairError::Overflow)?
                .checked_div(state.reserve_b)
                .ok_or(PairError::Overflow)?;

            liquidity = liquidity_a.min(liquidity_b);
        }

        if liquidity <= 0 {
            return Err(PairError::InsufficientLiquidityMinted);
        }

        lp_client.mint(&to, &liquidity);

        state.reserve_a = balance_a;
        state.reserve_b = balance_b;
        state.k_last = balance_a.checked_mul(balance_b).ok_or(PairError::Overflow)?;

        state.block_timestamp_last = env.ledger().timestamp();

        set_pair_state(&env, &state);

        PairEvents::mint(&env, &to, amount_a, amount_b);

        Ok(liquidity)
    }

    // ─────────────────────────────────────────
    // Burn
    // ─────────────────────────────────────────

    pub fn burn(env: Env, to: Address) -> Result<(i128, i128), PairError> {
        to.require_auth();

        let mut state = get_pair_state(&env).ok_or(PairError::NotInitialized)?;
        let contract = env.current_contract_address();

        let lp_balance = TokenClient::new(&env, &state.lp_token).balance(&contract);

        let total_supply = LpTokenClient::new(&env, &state.lp_token).total_supply();

        if total_supply == 0 {
            return Err(PairError::InsufficientLiquidityBurned);
        }

        let amount_a = lp_balance
            .checked_mul(state.reserve_a)
            .ok_or(PairError::Overflow)?
            .checked_div(total_supply)
            .ok_or(PairError::Overflow)?;

        let amount_b = lp_balance
            .checked_mul(state.reserve_b)
            .ok_or(PairError::Overflow)?
            .checked_div(total_supply)
            .ok_or(PairError::Overflow)?;

        if amount_a <= 0 || amount_b <= 0 {
            return Err(PairError::InsufficientLiquidityBurned);
        }

        LpTokenClient::new(&env, &state.lp_token).burn(&contract, &lp_balance);

        TokenClient::new(&env, &state.token_a).transfer(&contract, &to, &amount_a);

        TokenClient::new(&env, &state.token_b).transfer(&contract, &to, &amount_b);

        state.reserve_a = state.reserve_a.checked_sub(amount_a).ok_or(PairError::Overflow)?;

        state.reserve_b = state.reserve_b.checked_sub(amount_b).ok_or(PairError::Overflow)?;

        state.k_last = state.reserve_a.checked_mul(state.reserve_b).ok_or(PairError::Overflow)?;

        state.block_timestamp_last = env.ledger().timestamp();

        set_pair_state(&env, &state);

        PairEvents::burn(&env, &to, amount_a, amount_b, &to);

        Ok((amount_a, amount_b))
    }

    // ─────────────────────────────────────────
    // Burn single-side
    // ─────────────────────────────────────────

    pub fn burn_single_side(
        env: Env,
        to: Address,
        lp_amount: i128,
        preferred_token: Address,
        min_amount_out: i128,
    ) -> Result<i128, PairError> {
        to.require_auth();

        let _guard = reentrancy::ReentrancyGuard::acquire(&env)?;

        let mut state = get_pair_state(&env).ok_or(PairError::NotInitialized)?;
        let fee_state = get_fee_state(&env).ok_or(PairError::NotInitialized)?;

        if lp_amount <= 0 || min_amount_out <= 0 {
            return Err(PairError::InvalidInput);
        }

        let prefer_a = if preferred_token == state.token_a {
            true
        } else if preferred_token == state.token_b {
            false
        } else {
            return Err(PairError::InvalidInput);
        };

        let lp_client = LpTokenClient::new(&env, &state.lp_token);
        let total_supply = lp_client.total_supply();

        if total_supply == 0 {
            return Err(PairError::InsufficientLiquidityBurned);
        }

        // Burn LP from caller (total_supply read before burn)
        lp_client.burn(&to, &lp_amount);

        let share_a = lp_amount
            .checked_mul(state.reserve_a)
            .ok_or(PairError::Overflow)?
            .checked_div(total_supply)
            .ok_or(PairError::Overflow)?;

        let share_b = lp_amount
            .checked_mul(state.reserve_b)
            .ok_or(PairError::Overflow)?
            .checked_div(total_supply)
            .ok_or(PairError::Overflow)?;

        if share_a <= 0 || share_b <= 0 {
            return Err(PairError::InsufficientLiquidityBurned);
        }

        let (share_preferred, share_unwanted, reserve_preferred, reserve_unwanted) = if prefer_a {
            (share_a, share_b, state.reserve_a, state.reserve_b)
        } else {
            (share_b, share_a, state.reserve_b, state.reserve_a)
        };

        // Post-burn reserves are the baseline for the internal swap
        let reserve_preferred_post_burn = reserve_preferred
            .checked_sub(share_preferred)
            .ok_or(PairError::Overflow)?;
        let reserve_unwanted_post_burn = reserve_unwanted
            .checked_sub(share_unwanted)
            .ok_or(PairError::Overflow)?;

        let fee_bps = dynamic_fee::compute_fee_bps(&fee_state) as i128;
        let fee_factor = 10_000i128 - fee_bps;

        let amount_in_with_fee = share_unwanted
            .checked_mul(fee_factor)
            .ok_or(PairError::Overflow)?;

        let swap_numerator = amount_in_with_fee
            .checked_mul(reserve_preferred_post_burn)
            .ok_or(PairError::Overflow)?;

        let swap_denominator = reserve_unwanted_post_burn
            .checked_mul(10_000)
            .ok_or(PairError::Overflow)?
            .checked_add(amount_in_with_fee)
            .ok_or(PairError::Overflow)?;

        if swap_denominator == 0 {
            return Err(PairError::InsufficientLiquidity);
        }

        let swap_out = swap_numerator / swap_denominator;

        let total_out = share_preferred
            .checked_add(swap_out)
            .ok_or(PairError::Overflow)?;

        if total_out < min_amount_out {
            return Err(PairError::SlippageExceeded);
        }

        // Final reserves: unwanted is net-unchanged (burned share re-enters as swap input)
        let reserve_preferred_final = reserve_preferred_post_burn
            .checked_sub(swap_out)
            .ok_or(PairError::Overflow)?;
        let reserve_unwanted_final = reserve_unwanted;

        // K invariant check against post-burn baseline (swap leg must not violate K)
        let k_before = reserve_preferred_post_burn
            .checked_mul(reserve_unwanted_post_burn)
            .ok_or(PairError::Overflow)?
            .checked_mul(100_000_000)
            .ok_or(PairError::Overflow)?;

        let balance_preferred_adj = reserve_preferred_final
            .checked_mul(10_000)
            .ok_or(PairError::Overflow)?;

        let balance_unwanted_adj = reserve_unwanted_final
            .checked_mul(10_000)
            .ok_or(PairError::Overflow)?
            .checked_sub(
                share_unwanted.checked_mul(fee_bps).ok_or(PairError::Overflow)?,
            )
            .ok_or(PairError::Overflow)?;

        let k_after = balance_preferred_adj
            .checked_mul(balance_unwanted_adj)
            .ok_or(PairError::Overflow)?;

        if k_after < k_before {
            return Err(PairError::InvalidK);
        }

        let contract = env.current_contract_address();
        TokenClient::new(&env, &preferred_token).transfer(&contract, &to, &total_out);

        if prefer_a {
            state.reserve_a = reserve_preferred_final;
            state.reserve_b = reserve_unwanted_final;
        } else {
            state.reserve_a = reserve_unwanted_final;
            state.reserve_b = reserve_preferred_final;
        }

        state.k_last = state
            .reserve_a
            .checked_mul(state.reserve_b)
            .ok_or(PairError::Overflow)?;
        state.block_timestamp_last = env.ledger().timestamp();

        set_pair_state(&env, &state);

        PairEvents::burn_single_side(&env, &to, lp_amount, &preferred_token, total_out);

        Ok(total_out)
    }

    // ─────────────────────────────────────────
    // Swap
    // ─────────────────────────────────────────

    pub fn swap(
        env: Env,
        amount_a_out: i128,
        amount_b_out: i128,
        to: Address,
    ) -> Result<(), PairError> {
        let _guard = reentrancy::ReentrancyGuard::acquire(&env)?;
        Self::swap_inner(&env, amount_a_out, amount_b_out, &to)
    }

    // ─────────────────────────────────────────
    // Flash loan
    // ─────────────────────────────────────────

    /// Borrows `amount_a` / `amount_b` from pool reserves, invokes `receiver.on_flash_loan`,
    /// then verifies repayment (principal + fee) before returning.
    pub fn flash_loan(
        env: Env,
        receiver: Address,
        amount_a: i128,
        amount_b: i128,
        data: Bytes,
    ) -> Result<(), PairError> {
        flash_loan::execute_flash_loan(&env, &receiver, amount_a, amount_b, &data)
    }

    // ─────────────────────────────────────────
    // Views
    // ─────────────────────────────────────────

    /// Returns (reserve_a, reserve_b, block_timestamp_last).
    pub fn get_reserves(env: Env) -> Result<(i128, i128, u64), PairError> {
        let state = get_pair_state(&env).ok_or(PairError::NotInitialized)?;
        Ok((state.reserve_a, state.reserve_b, state.block_timestamp_last))
    }

    /// Consults the oracle for a TWAP over a given window.
    pub fn consult_twap(env: Env, window_ledgers: u32) -> Result<(i128, i128), errors::OracleError> {
        oracle::consult_twap(&env, window_ledgers)
    }

    /// Returns the LP token address.
    pub fn lp_token(env: Env) -> Result<Address, PairError> {
        let state = get_pair_state(&env).ok_or(PairError::NotInitialized)?;
        Ok(state.lp_token)
    }

    /// Returns the current dynamic fee in basis points.
    pub fn get_current_fee_bps(env: Env) -> u32 {
        match get_fee_state(&env) {
            Some(fs) => compute_fee_bps(&fs),
            None => 30,
        }
    }

    // ─────────────────────────────────────────
    // Sync
    // ─────────────────────────────────────────

    /// Syncs reserves to actual token balances and updates the oracle timestamp.
    pub fn sync(env: Env) -> Result<(), PairError> {
        let mut state = get_pair_state(&env).ok_or(PairError::NotInitialized)?;
        let contract = env.current_contract_address();

        let balance_a = TokenClient::new(&env, &state.token_a).balance(&contract);
        let balance_b = TokenClient::new(&env, &state.token_b).balance(&contract);

        state.reserve_a = balance_a;
        state.reserve_b = balance_b;
        state.block_timestamp_last = env.ledger().timestamp();
        state.k_last = balance_a.checked_mul(balance_b).ok_or(PairError::Overflow)?;

        set_pair_state(&env, &state);

        PairEvents::sync(&env, balance_a, balance_b);

        Ok(())
    }

    fn swap_inner(
        env: &Env,
        amount_a_out: i128,
        amount_b_out: i128,
        to: &Address,
    ) -> Result<(), PairError> {
        if amount_a_out <= 0 && amount_b_out <= 0 {
            return Err(PairError::InsufficientOutputAmount);
        }

        let mut pair = get_pair_state(env).ok_or(PairError::NotInitialized)?;
        let mut fee_state = get_fee_state(env).ok_or(PairError::NotInitialized)?;

        if amount_a_out >= pair.reserve_a || amount_b_out >= pair.reserve_b {
            return Err(PairError::InsufficientLiquidity);
        }

        // Store pre-swap reserves for price delta calculation
        let reserve_a_before = pair.reserve_a;
        let reserve_b_before = pair.reserve_b;

        dynamic_fee::decay_stale_ema(env, &mut fee_state);
        let fee_bps = dynamic_fee::compute_fee_bps(&fee_state);

        let contract_address = env.current_contract_address();

        if amount_a_out > 0 {
            TokenClient::new(env, &pair.token_a).transfer(&contract_address, to, &amount_a_out);
        }

        if amount_b_out > 0 {
            TokenClient::new(env, &pair.token_b).transfer(&contract_address, to, &amount_b_out);
        }

        let balance_a = TokenClient::new(env, &pair.token_a).balance(&contract_address);

        let balance_b = TokenClient::new(env, &pair.token_b).balance(&contract_address);

        let amount_a_in = (balance_a - (pair.reserve_a - amount_a_out)).max(0);

        let amount_b_in = (balance_b - (pair.reserve_b - amount_b_out)).max(0);

        if amount_a_in <= 0 && amount_b_in <= 0 {
            return Err(PairError::InsufficientInputAmount);
        }

        let fee = fee_bps as i128;

        let balance_a_adj = balance_a
            .checked_mul(10_000)
            .ok_or(PairError::Overflow)?
            .checked_sub(amount_a_in * fee)
            .ok_or(PairError::Overflow)?;

        let balance_b_adj = balance_b
            .checked_mul(10_000)
            .ok_or(PairError::Overflow)?
            .checked_sub(amount_b_in * fee)
            .ok_or(PairError::Overflow)?;

        let k_before = pair
            .reserve_a
            .checked_mul(pair.reserve_b)
            .ok_or(PairError::Overflow)?
            .checked_mul(100_000_000)
            .ok_or(PairError::Overflow)?;

        let k_after = balance_a_adj.checked_mul(balance_b_adj).ok_or(PairError::Overflow)?;

        if k_after < k_before {
            return Err(PairError::InvalidK);
        }

        pair.reserve_a = balance_a;
        pair.reserve_b = balance_b;
        pair.k_last = balance_a.checked_mul(balance_b).ok_or(PairError::Overflow)?;

        pair.block_timestamp_last = env.ledger().timestamp();

        // --- Update volatility tracking after reserves change ---
        // Compute price delta as the absolute change in the price ratio.
        // price_before = reserve_b_before / reserve_a_before (scaled)
        // price_after  = reserve_b / reserve_a (scaled)
        // price_delta  = |price_after - price_before| (scaled)
        const SCALE: i128 = 100_000_000_000_000; // 1e14, matches dynamic_fee::SCALE
        
        if reserve_a_before > 0 && reserve_b_before > 0 && pair.reserve_a > 0 && pair.reserve_b > 0 {
            // price_before = (reserve_b_before * SCALE) / reserve_a_before
            let price_before = reserve_b_before
                .checked_mul(SCALE)
                .and_then(|v| v.checked_div(reserve_a_before))
                .unwrap_or(0);
            
            // price_after = (reserve_b * SCALE) / reserve_a
            let price_after = pair.reserve_b
                .checked_mul(SCALE)
                .and_then(|v| v.checked_div(pair.reserve_a))
                .unwrap_or(0);
            
            // price_delta_abs = |price_after - price_before|
            let price_delta_abs = if price_after > price_before {
                price_after - price_before
            } else {
                price_before - price_after
            };
            
            // trade_size = total input amount (in terms of reserve A equivalent)
            // For simplicity, use the larger of the two inputs
            let trade_size = amount_a_in.max(amount_b_in);
            
            // total_reserve = reserve_a + reserve_b (simple sum for size weighting)
            let total_reserve = pair.reserve_a.saturating_add(pair.reserve_b);
            
            // Update volatility (ignore errors to not break swaps on edge cases)
            let _ = dynamic_fee::update_volatility(
                env,
                &mut fee_state,
                price_delta_abs,
                trade_size,
                total_reserve,
            );
        }

        set_pair_state(env, &pair);
        set_fee_state(env, &fee_state);

        PairEvents::swap(
            env,
            to,
            amount_a_in,
            amount_b_in,
            amount_a_out,
            amount_b_out,
            fee_bps,
            to,
        );

        Ok(())
    }
}
