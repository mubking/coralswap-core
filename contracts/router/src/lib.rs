#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod errors;
mod helpers;
mod storage;

#[cfg(test)]
mod test;

use errors::RouterError;
use helpers::{
    compute_optimal_amounts, get_amount_in, get_amount_out, get_pair_address,
    get_pair_reserves_and_fee, get_path_amounts_out, sort_tokens, PairClient,
};
use soroban_sdk::{contract, contractimpl, token::TokenClient, Address, Env, Vec};
use storage::{get_factory, get_hubs, set_factory, set_hubs};

#[contract]
pub struct Router;

#[contractimpl]
impl Router {
    pub fn initialize(env: Env, factory: Address, hubs: Vec<Address>) {
        set_factory(&env, &factory);
        set_hubs(&env, &hubs);
    }

    /// Stores the hub token addresses used for multi-hop path discovery.
    pub fn set_hubs(env: Env, hubs: Vec<Address>) {
        set_hubs(&env, &hubs);
    }

    /// Returns the current list of hub token addresses.
    pub fn get_hubs(env: Env) -> Vec<Address> {
        get_hubs(&env)
    }

    /// Finds the best multi-hop route for swapping token_in → token_out.
    /// Evaluates 1-hop (direct), 2-hop (via each hub), and 3-hop (via each
    /// ordered hub pair) paths, selecting the one with highest expected output.
    pub fn get_best_path(
        env: Env,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
    ) -> Result<(Vec<Address>, i128), RouterError> {
        if token_in == token_out {
            return Err(RouterError::IdenticalTokens);
        }
        if amount_in <= 0 {
            return Err(RouterError::ZeroAmount);
        }

        let factory = get_factory(&env).ok_or(RouterError::PairNotFound)?;
        let hubs = get_hubs(&env);

        // Track best across all candidate paths.
        let mut best_path: Vec<Address> = Vec::new(&env);
        let mut best_out: i128 = -1;

        // 1) Direct pair (1 hop)
        if let Ok(pair) = get_pair_address(&env, &factory, &token_in, &token_out) {
            if let Ok((r_in, r_out, fee)) =
                get_pair_reserves_and_fee(&env, &pair, &token_in, &token_out)
            {
                if let Ok(out) = get_amount_out(&env, amount_in, r_in, r_out, fee) {
                    let mut path = Vec::new(&env);
                    path.push_back(token_in.clone());
                    path.push_back(token_out.clone());
                    best_path = path;
                    best_out = out;
                }
            }
        }

        // 2) 2-hop routes: token_in → hub → token_out
        for i in 0..hubs.len() {
            let hub = hubs.get(i).unwrap();
            if hub == token_in || hub == token_out {
                continue;
            }
            let pair_1 = get_pair_address(&env, &factory, &token_in, &hub);
            let pair_2 = get_pair_address(&env, &factory, &hub, &token_out);
            if let (Ok(p1), Ok(p2)) = (pair_1, pair_2) {
                if let (Ok((r1_in, r1_out, f1)), Ok((r2_in, r2_out, f2))) = (
                    get_pair_reserves_and_fee(&env, &p1, &token_in, &hub),
                    get_pair_reserves_and_fee(&env, &p2, &hub, &token_out),
                ) {
                    if let Ok(mid) = get_amount_out(&env, amount_in, r1_in, r1_out, f1) {
                        if let Ok(out) = get_amount_out(&env, mid, r2_in, r2_out, f2) {
                            if out > best_out {
                                let mut path = Vec::new(&env);
                                path.push_back(token_in.clone());
                                path.push_back(hub.clone());
                                path.push_back(token_out.clone());
                                best_path = path;
                                best_out = out;
                            }
                        }
                    }
                }
            }
        }

        // 3) 3-hop routes: token_in → hub_i → hub_j → token_out
        for i in 0..hubs.len() {
            for j in 0..hubs.len() {
                if i == j {
                    continue;
                }
                let hub_i = hubs.get(i).unwrap();
                let hub_j = hubs.get(j).unwrap();
                if hub_i == token_in
                    || hub_i == token_out
                    || hub_j == token_in
                    || hub_j == token_out
                {
                    continue;
                }
                let p1 = get_pair_address(&env, &factory, &token_in, &hub_i);
                let p2 = get_pair_address(&env, &factory, &hub_i, &hub_j);
                let p3 = get_pair_address(&env, &factory, &hub_j, &token_out);
                if let (Ok(p1), Ok(p2), Ok(p3)) = (p1, p2, p3) {
                    if let (
                        Ok((r1_in, r1_out, f1)),
                        Ok((r2_in, r2_out, f2)),
                        Ok((r3_in, r3_out, f3)),
                    ) = (
                        get_pair_reserves_and_fee(&env, &p1, &token_in, &hub_i),
                        get_pair_reserves_and_fee(&env, &p2, &hub_i, &hub_j),
                        get_pair_reserves_and_fee(&env, &p3, &hub_j, &token_out),
                    ) {
                        if let Ok(mid1) = get_amount_out(&env, amount_in, r1_in, r1_out, f1) {
                            if let Ok(mid2) = get_amount_out(&env, mid1, r2_in, r2_out, f2) {
                                if let Ok(out) = get_amount_out(&env, mid2, r3_in, r3_out, f3) {
                                    if out > best_out {
                                        let mut path = Vec::new(&env);
                                        path.push_back(token_in.clone());
                                        path.push_back(hub_i.clone());
                                        path.push_back(hub_j.clone());
                                        path.push_back(token_out.clone());
                                        best_path = path;
                                        best_out = out;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if best_out < 0 {
            return Err(RouterError::PairNotFound);
        }
        Ok((best_path, best_out))
    }

    /// Swaps an exact amount of input tokens for a minimum amount of output
    /// tokens along a given multi-hop path. Supports 1, 2, or 3 hops.
    ///
    /// The path must have 2 to 4 entries: [token_in, ..., token_out].
    /// Intermediate tokens are sent to and forwarded by this router contract.
    pub fn swap_exact_tokens_multi_hop(
        env: Env,
        path: Vec<Address>,
        amount_in: i128,
        amount_out_min: i128,
        to: Address,
        deadline: u64,
    ) -> Result<i128, RouterError> {
        if deadline < env.ledger().timestamp() {
            return Err(RouterError::Expired);
        }
        if amount_in <= 0 {
            return Err(RouterError::ZeroAmount);
        }
        let hops = path.len() - 1;
        if !(1..=3).contains(&hops) {
            return Err(RouterError::InvalidPath);
        }

        let factory = get_factory(&env).ok_or(RouterError::PairNotFound)?;
        let amounts = get_path_amounts_out(&env, &factory, &path, amount_in)?;
        let final_out = amounts.get(amounts.len() - 1).unwrap();

        if final_out < amount_out_min {
            return Err(RouterError::InsufficientOutputAmount);
        }

        to.require_auth();
        let router = env.current_contract_address();

        // Transfer user input to the first pair
        let token_in = path.get(0).unwrap();
        let first_pair = get_pair_address(&env, &factory, &token_in, &path.get(1).unwrap())?;
        TokenClient::new(&env, &token_in).transfer(&to, &first_pair, &amount_in);

        // Execute each hop in sequence
        for i in 0..hops {
            let token_from = path.get(i).unwrap();
            let token_to = path.get(i + 1).unwrap();
            let pair = get_pair_address(&env, &factory, &token_from, &token_to)?;
            let amount_out_hop = amounts.get(i).unwrap();
            let dest = if i + 1 == hops { &to } else { &router };

            let (token_0, _) = sort_tokens(&token_from, &token_to)?;
            let pair_client = PairClient::new(&env, &pair);
            if token_from == token_0 {
                pair_client.swap(&0, &amount_out_hop, dest);
            } else {
                pair_client.swap(&amount_out_hop, &0, dest);
            }

            // Forward the intermediate output to the next pair
            if i + 1 < hops {
                let next_pair =
                    get_pair_address(&env, &factory, &token_to, &path.get(i + 2).unwrap())?;
                TokenClient::new(&env, &token_to).transfer(&router, &next_pair, &amount_out_hop);
            }
        }

        Ok(final_out)
    }

    /// Swaps an exact amount of input tokens for a minimum amount of output
    /// tokens along a given path. Supports 1, 2, or 3 hops.
    pub fn swap_exact_tokens_for_tokens(
        env: Env,
        amount_in: i128,
        amount_out_min: i128,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Result<Vec<i128>, RouterError> {
        let final_out = Self::swap_exact_tokens_multi_hop(
            env.clone(),
            path,
            amount_in,
            amount_out_min,
            to,
            deadline,
        )?;
        let mut amounts = Vec::new(&env);
        amounts.push_back(final_out);
        Ok(amounts)
    }

    /// Swaps tokens to receive an exact amount of output tokens.
    /// Computes required input along the given path and enforces amount_in_max.
    pub fn swap_tokens_for_exact_tokens(
        env: Env,
        amount_out: i128,
        amount_in_max: i128,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Result<Vec<i128>, RouterError> {
        if deadline < env.ledger().timestamp() {
            return Err(RouterError::Expired);
        }
        if amount_out <= 0 {
            return Err(RouterError::ZeroAmount);
        }
        let hops = path.len() - 1;
        if !(1..=3).contains(&hops) {
            return Err(RouterError::InvalidPath);
        }

        let factory = get_factory(&env).ok_or(RouterError::PairNotFound)?;

        // Walk backwards: compute required input for each hop from final output
        let mut required = Vec::new(&env);
        let mut current = amount_out;
        for i in (0..hops).rev() {
            let pair =
                get_pair_address(&env, &factory, &path.get(i).unwrap(), &path.get(i + 1).unwrap())?;
            let (reserve_in, reserve_out, fee_bps) = get_pair_reserves_and_fee(
                &env,
                &pair,
                &path.get(i).unwrap(),
                &path.get(i + 1).unwrap(),
            )?;
            current = get_amount_in(&env, current, reserve_in, reserve_out, fee_bps)?;
            required.insert(0, current);
        }
        let amount_in_needed = current;

        if amount_in_needed > amount_in_max {
            return Err(RouterError::ExcessiveInputAmount);
        }

        to.require_auth();
        let router = env.current_contract_address();

        // Transfer user input to the first pair
        let token_in = path.get(0).unwrap();
        let first_pair = get_pair_address(&env, &factory, &token_in, &path.get(1).unwrap())?;
        TokenClient::new(&env, &token_in).transfer(&to, &first_pair, &amount_in_needed);

        // Execute each hop forward
        for i in 0..hops {
            let token_from = path.get(i).unwrap();
            let token_to = path.get(i + 1).unwrap();
            let pair = get_pair_address(&env, &factory, &token_from, &token_to)?;
            let _amount_in_this = required.get(i).unwrap();
            let dest = if i + 1 == hops { &to } else { &router };

            let (token_0, _) = sort_tokens(&token_from, &token_to)?;
            let pair_client = PairClient::new(&env, &pair);

            let out_expected = if i + 1 < hops { required.get(i + 1).unwrap() } else { amount_out };

            if token_from == token_0 {
                pair_client.swap(&0, &out_expected, dest);
            } else {
                pair_client.swap(&out_expected, &0, dest);
            }

            // Forward intermediate output to the next pair
            if i + 1 < hops {
                let next_pair =
                    get_pair_address(&env, &factory, &token_to, &path.get(i + 2).unwrap())?;
                TokenClient::new(&env, &token_to).transfer(&router, &next_pair, &out_expected);
            }
        }

        let mut result = Vec::new(&env);
        result.push_back(amount_in_needed);
        Ok(result)
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
