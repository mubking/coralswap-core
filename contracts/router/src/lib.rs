#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod errors;
mod helpers;
mod storage;

#[cfg(test)]
mod test;

use errors::RouterError;
use helpers::{compute_optimal_amounts, get_amount_in, get_amount_out, get_pair_address, sort_tokens, PairClient};
use soroban_sdk::{contract, contractimpl, token::TokenClient, Address, Env, Vec};
use storage::{get_factory, set_factory};

#[contract]
pub struct Router;

#[contractimpl]
impl Router {
    pub fn initialize(env: Env, factory: Address) {
        set_factory(&env, &factory);
    }
    pub fn swap_exact_tokens_for_tokens(
        env: Env,
        amount_in: i128,
        amount_out_min: i128,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Result<Vec<i128>, RouterError> {
        // Check deadline (ledger sequence)
        if env.ledger().sequence() > deadline as u32 {
            return Err(RouterError::TransactionExpired);
        }

        if amount_in <= 0 {
            return Err(RouterError::ZeroAmount);
        }

        if path.len() < 2 {
            return Err(RouterError::InvalidPath);
        }

        let factory = get_factory(&env).ok_or(RouterError::PairNotFound)?;
        let mut amounts = Vec::new(&env);
        amounts.push_back(amount_in);

        for i in 0..path.len() - 1 {
            let token_in = path.get(i).unwrap();
            let token_out = path.get(i + 1).unwrap();

            let pair_address = get_pair_address(&env, &factory, &token_in, &token_out)?;
            let pair_client = PairClient::new(&env, &pair_address);
            let (reserve_a, reserve_b, _) = pair_client.get_reserves();
            let fee_bps = pair_client.get_current_fee_bps();

            let (reserve_in, reserve_out) =
                if token_in < token_out { (reserve_a, reserve_b) } else { (reserve_b, reserve_a) };

            let amount_out = get_amount_out(&env, amounts.get(i).unwrap(), reserve_in, reserve_out, fee_bps)?;
            amounts.push_back(amount_out);

            if i == path.len() - 2 {
                // Last hop — output must meet minimum
                if amount_out < amount_out_min {
                    return Err(RouterError::SlippageExceeded);
                }

                // Determine (amount_a_out, amount_b_out) based on pair canonical ordering
                let (sorted_a, _) = sort_tokens(&token_in, &token_out)?;
                let (amount_a_out, amount_b_out) = if token_in == sorted_a {
                    (0i128, amount_out)
                } else {
                    (amount_out, 0i128)
                };

                // Transfer input tokens from user to the pair
                let amount_in_this_hop = amounts.get(i).unwrap();
                to.require_auth();
                TokenClient::new(&env, &token_in).transfer(&to, &pair_address, &amount_in_this_hop);

                pair_client.swap(&amount_a_out, &amount_b_out, &to);
            } else {
                // Intermediate hop: transfer to the next pair, the output goes to the next pair
                // For simplicity in single-hop paths this won't be reached
                let amount_in_this_hop = amounts.get(i).unwrap();
                to.require_auth();
                TokenClient::new(&env, &token_in).transfer(&to, &pair_address, &amount_in_this_hop);

                let next_pair_address =
                    get_pair_address(&env, &factory, &token_out, &path.get(i + 2).unwrap())?;

                let (sorted_a, _) = sort_tokens(&token_in, &token_out)?;
                let (amount_a_out, amount_b_out) = if token_in == sorted_a {
                    (0i128, amount_out)
                } else {
                    (amount_out, 0i128)
                };

                pair_client.swap(&amount_a_out, &amount_b_out, &next_pair_address);
            }
        }

        Ok(amounts)
    }

    /// Swaps tokens to receive an exact amount of output tokens.
    pub fn swap_tokens_for_exact_tokens(
        env: Env,
        amount_out: i128,
        amount_in_max: i128,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Result<Vec<i128>, RouterError> {
        // Check deadline (ledger sequence)
        if env.ledger().sequence() > deadline as u32 {
            return Err(RouterError::TransactionExpired);
        }

        if amount_out <= 0 {
            return Err(RouterError::ZeroAmount);
        }

        if path.len() < 2 {
            return Err(RouterError::InvalidPath);
        }

        let factory = get_factory(&env).ok_or(RouterError::PairNotFound)?;

        // Compute required input amounts backwards through the path
        let mut amounts = Vec::new(&env);
        amounts.push_back(amount_out);

        for i in (0..path.len() - 1).rev() {
            let token_in = path.get(i).unwrap();
            let token_out = path.get(i + 1).unwrap();

            let pair_address = get_pair_address(&env, &factory, &token_in, &token_out)?;
            let pair_client = PairClient::new(&env, &pair_address);
            let (reserve_a, reserve_b, _) = pair_client.get_reserves();
            let fee_bps = pair_client.get_current_fee_bps();

            let (reserve_in, reserve_out) =
                if token_in < token_out { (reserve_a, reserve_b) } else { (reserve_b, reserve_a) };

            let amount_in =
                get_amount_in(&env, amounts.get(0).unwrap(), reserve_in, reserve_out, fee_bps)?;
            amounts.insert(0, amount_in);
        }

        let total_amount_in = amounts.get(0).unwrap();
        if total_amount_in > amount_in_max {
            return Err(RouterError::ExcessiveInputAmount);
        }

        // Execute swaps forward
        to.require_auth();

        for i in 0..path.len() - 1 {
            let token_in = path.get(i).unwrap();
            let token_out = path.get(i + 1).unwrap();
            let amount_in_this = amounts.get(i).unwrap();
            let amount_out_this = amounts.get(i + 1).unwrap();

            let pair_address = get_pair_address(&env, &factory, &token_in, &token_out)?;
            let pair_client = PairClient::new(&env, &pair_address);

            TokenClient::new(&env, &token_in).transfer(&to, &pair_address, &amount_in_this);

            let recipient = if i == path.len() - 2 {
                to.clone()
            } else {
                get_pair_address(&env, &factory, &token_out, &path.get(i + 2).unwrap())?
            };

            let (sorted_a, _) = sort_tokens(&token_in, &token_out)?;
            let (amount_a_out, amount_b_out) = if token_in == sorted_a {
                (0i128, amount_out_this)
            } else {
                (amount_out_this, 0i128)
            };

            pair_client.swap(&amount_a_out, &amount_b_out, &recipient);
        }

        Ok(amounts)
    }

    /// Adds liquidity to a token pair (not yet implemented).
    ///
    /// # Arguments
    /// * `token_a` - First token address
    /// * `token_b` - Second token address
    /// * `amount_a_desired` - Desired amount of token_a to add
    /// * `amount_b_desired` - Desired amount of token_b to add
    /// * `amount_a_min` - Minimum amount of token_a to add
    /// * `amount_b_min` - Minimum amount of token_b to add
    /// * `to` - Recipient of LP tokens
    /// * `deadline` - Unix timestamp after which the transaction will revert
    pub fn add_liquidity(
        env: Env,
        token_a: Address,
        token_b: Address,
        amount_a_desired: i128,
        amount_b_desired: i128,
        amount_a_min: i128,
        amount_b_min: i128,
        to: Address,
        deadline: u64,
    ) -> Result<(i128, i128, i128), RouterError> {
        // Check deadline
        if deadline < env.ledger().timestamp() {
            return Err(RouterError::Expired);
        }

        // Validate inputs: reject zero desired amounts
        if amount_a_desired <= 0 || amount_b_desired <= 0 {
            return Err(RouterError::ZeroAmount);
        }

        // Validate inputs: reject identical tokens
        if token_a == token_b {
            return Err(RouterError::IdenticalTokens);
        }

        // Get factory address
        let factory = get_factory(&env).ok_or(RouterError::PairNotFound)?;

        // Get pair address from factory
        let pair_address = get_pair_address(&env, &factory, &token_a, &token_b)?;

        // Get pair contract client and current reserves
        let pair_client = PairClient::new(&env, &pair_address);
        let (reserve_a, reserve_b, _) = pair_client.get_reserves();

        // Calculate optimal deposit amounts preserving pool ratio
        let (amount_a, amount_b) = compute_optimal_amounts(
            amount_a_desired,
            amount_b_desired,
            amount_a_min,
            amount_b_min,
            reserve_a,
            reserve_b,
        )?;

        // The user must provide authorization for token transfers
        to.require_auth();

        // Transfer tokens from 'to' to the pair contract
        TokenClient::new(&env, &token_a).transfer(&to, &pair_address, &amount_a);
        TokenClient::new(&env, &token_b).transfer(&to, &pair_address, &amount_b);

        // Mint LP tokens to the recipient
        let liquidity = pair_client.mint(&to);

        Ok((amount_a, amount_b, liquidity))
    }

    /// Removes liquidity from a token pair (not yet implemented).
    ///
    /// # Arguments
    /// * `token_a` - First token address
    /// * `token_b` - Second token address
    /// * `liquidity` - Amount of LP tokens to burn
    /// * `amount_a_min` - Minimum amount of token_a to receive
    /// * `amount_b_min` - Minimum amount of token_b to receive
    /// * `to` - Recipient of underlying tokens
    /// * `deadline` - Unix timestamp after which the transaction will revert
    pub fn remove_liquidity(
        env: Env,
        token_a: Address,
        token_b: Address,
        liquidity: i128,
        amount_a_min: i128,
        amount_b_min: i128,
        to: Address,
        deadline: u64,
    ) -> Result<(i128, i128), RouterError> {
        // Check deadline
        if deadline < env.ledger().timestamp() {
            return Err(RouterError::Expired);
        }

        // Check for non-zero liquidity
        if liquidity <= 0 {
            return Err(RouterError::ZeroAmount);
        }

        // Check for identical tokens
        if token_a == token_b {
            return Err(RouterError::IdenticalTokens);
        }

        // Get factory address
        let factory = get_factory(&env).ok_or(RouterError::PairNotFound)?;

        // Get pair address
        let pair_address = get_pair_address(&env, &factory, &token_a, &token_b)?;

        // Get pair contract client
        let pair_client = PairClient::new(&env, &pair_address);

        // Get LP token address from pair
        let lp_token_address = pair_client.lp_token();

        // The user must provide authorization for the Router to transfer LP tokens
        to.require_auth();

        // Transfer LP tokens from 'to' to pair
        let lp_token_client = TokenClient::new(&env, &lp_token_address);
        lp_token_client.transfer(&to, &pair_address, &liquidity);

        // Call Pair::burn(to) - this will burn LP tokens from the pair and transfer underlying tokens
        let (amount_a, amount_b) = pair_client.burn(&to);

        // Enforce minimum output amounts
        if amount_a < amount_a_min || amount_b < amount_b_min {
            return Err(RouterError::InsufficientOutputAmount);
        }

        Ok((amount_a, amount_b))
    }
}
