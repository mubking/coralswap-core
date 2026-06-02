#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::Address as _,
    token::{Client as TokenClient, StellarAssetClient},
    Address, BytesN, Env, Vec,
};

// ----------------------------------------------------------------
// WASM bytes — loaded at compile time from workspace build outputs
// All paths are relative to the workspace root Cargo.toml
// ----------------------------------------------------------------
mod wasm {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32-unknown-unknown/release/coralswap_factory.wasm"
    );
    pub type FactoryClient<'a> = Client<'a>;
}

mod pair_wasm {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32-unknown-unknown/release/coralswap_pair.wasm"
    );
    pub type PairClient<'a> = Client<'a>;
}

mod lp_wasm {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32-unknown-unknown/release/coralswap_lp_token.wasm"
    );
    pub type LpClient<'a> = Client<'a>;
}

// ----------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------

fn mint(env: &Env, asset: &StellarAssetClient, to: &Address, amount: i128) {
    asset.mint(to, &amount);
}

/// Deploy the factory and return (factory_address, FactoryClient).
/// initialize() returns Result<(), FactoryError> so we call the try_ variant
/// and unwrap the outer Result (RPC error) then the inner Result (contract error).
fn deploy_factory<'a>(
    env: &'a Env,
    pair_hash: BytesN<32>,
    lp_hash: BytesN<32>,
    admin: &Address,
) -> (Address, wasm::FactoryClient<'a>) {
    let factory_addr = env.register_contract_wasm(None, wasm::WASM);
    let client = wasm::FactoryClient::new(env, &factory_addr);
    let signers = Vec::from_array(env, [admin.clone()]);
    // initialize returns Result<(), FactoryError> — use try_ and unwrap both layers
    client
        .try_initialize(&signers, &pair_hash, &lp_hash, admin)
        .unwrap()  // outer: RPC / host error
        .unwrap(); // inner: FactoryError
    (factory_addr, client)
}

// ----------------------------------------------------------------
// Full lifecycle test
// ----------------------------------------------------------------

#[test]
fn test_factory_full_pool_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();

    // ── 1. Accounts ──────────────────────────────────────────────
    let admin = Address::generate(&env);
    let lp_provider = Address::generate(&env);
    let swapper = Address::generate(&env);

    // ── 2. Deploy token contracts ────────────────────────────────
    let token_a_id = env.register_stellar_asset_contract(admin.clone());
    let token_b_id = env.register_stellar_asset_contract(admin.clone());

    // Canonical sort: factory sorts tokens the same way
    let (token_0_id, token_1_id) = if token_a_id < token_b_id {
        (token_a_id.clone(), token_b_id.clone())
    } else {
        (token_b_id.clone(), token_a_id.clone())
    };

    let token_0_asset = StellarAssetClient::new(&env, &token_0_id);
    let token_1_asset = StellarAssetClient::new(&env, &token_1_id);
    let token_0 = TokenClient::new(&env, &token_0_id);
    let token_1 = TokenClient::new(&env, &token_1_id);

    // ── 3. Upload WASM hashes & deploy factory ───────────────────
    let pair_hash = env.deployer().upload_contract_wasm(pair_wasm::WASM);
    let lp_hash = env.deployer().upload_contract_wasm(lp_wasm::WASM);

    let (_, factory) = deploy_factory(&env, pair_hash, lp_hash, &admin);

    // Post-deploy assertions
    // get_pair_count() returns u32 directly (no Result)
    assert_eq!(factory.get_pair_count(), 0, "pair count should be 0 after init");
    // is_paused() returns bool directly (no Result)
    assert!(!factory.is_paused(), "factory should not be paused after init");

    // ── 4. Create pair ───────────────────────────────────────────
    // create_pair returns Result<Address, FactoryError>
    let pair_addr = factory
        .try_create_pair(&token_0_id, &token_1_id)
        .unwrap()  // outer
        .unwrap(); // inner

    // get_pair returns Option<Address> directly
    assert_eq!(
        factory.get_pair(&token_0_id, &token_1_id),
        Some(pair_addr.clone()),
        "get_pair(0,1) should return pair"
    );
    assert_eq!(
        factory.get_pair(&token_1_id, &token_0_id),
        Some(pair_addr.clone()),
        "get_pair(1,0) should return same pair"
    );
    assert_eq!(factory.get_pair_count(), 1, "pair count should be 1");

    // Duplicate pair must be rejected
    assert!(
        factory.try_create_pair(&token_0_id, &token_1_id).unwrap().is_err(),
        "duplicate pair creation must fail"
    );

    let pair = pair_wasm::PairClient::new(&env, &pair_addr);

    // get_reserves returns Result<(i128, i128, u64), PairError>
    let (res_a, res_b, _) = pair.try_get_reserves().unwrap().unwrap();
    assert_eq!(res_a, 0, "reserve_a should be 0 after creation");
    assert_eq!(res_b, 0, "reserve_b should be 0 after creation");

    // ── 5. Fund LP provider & first deposit (token_0-dominant) ───
    let deposit_0: i128 = 1_000_000_000;
    let deposit_1: i128 = 2_000_000_000;

    mint(&env, &token_0_asset, &lp_provider, deposit_0);
    mint(&env, &token_1_asset, &lp_provider, deposit_1);

    token_0.transfer(&lp_provider, &pair_addr, &deposit_0);
    token_1.transfer(&lp_provider, &pair_addr, &deposit_1);

    // mint returns Result<i128, PairError>
    let lp_minted = pair.try_mint(&lp_provider).unwrap().unwrap();
    assert!(lp_minted > 0, "LP minted must be positive");

    // lp_token() returns Result<Address, PairError>
    let lp_token_addr = pair.try_lp_token().unwrap().unwrap();
    let lp_token = lp_wasm::LpClient::new(&env, &lp_token_addr);

    // total_supply() returns i128 directly (no Result)
    let lp_supply_after_mint = lp_token.total_supply();

    // balance() returns i128 directly (no Result)
    assert_eq!(
        lp_token.balance(&lp_provider),
        lp_minted,
        "LP provider balance should equal minted amount"
    );
    assert!(
        lp_supply_after_mint > lp_minted,
        "total supply must include MINIMUM_LIQUIDITY lock"
    );

    let (res_a, res_b, _) = pair.try_get_reserves().unwrap().unwrap();
    assert_eq!(res_a, deposit_0, "reserve_a should equal deposit_0");
    assert_eq!(res_b, deposit_1, "reserve_b should equal deposit_1");

    // K invariant
    assert_eq!(res_a.checked_mul(res_b).unwrap(), res_a * res_b);

    // ── 6. Second deposit (token_1-dominant ratio) ────────────────
    let deposit_0b: i128 = 500_000_000;
    let deposit_1b: i128 = 1_000_000_000;

    mint(&env, &token_0_asset, &lp_provider, deposit_0b);
    mint(&env, &token_1_asset, &lp_provider, deposit_1b);

    token_0.transfer(&lp_provider, &pair_addr, &deposit_0b);
    token_1.transfer(&lp_provider, &pair_addr, &deposit_1b);

    let lp_minted_2 = pair.try_mint(&lp_provider).unwrap().unwrap();
    assert!(lp_minted_2 > 0, "second LP minted must be positive");

    let lp_supply_after_second_mint = lp_token.total_supply();
    assert!(
        lp_supply_after_second_mint > lp_supply_after_mint,
        "total supply must increase after second mint"
    );

    // ── 7. Swap: token_0 in, token_1 out ─────────────────────────
    let swap_amount_in: i128 = 100_000_000;

    let (reserve_a_pre, reserve_b_pre, _) = pair.try_get_reserves().unwrap().unwrap();

    // get_current_fee_bps() returns u32 directly
    let fee_bps = pair.get_current_fee_bps() as i128;
    let amount_in_with_fee = swap_amount_in * (10_000 - fee_bps);
    let expected_out =
        (amount_in_with_fee * reserve_b_pre) / (reserve_a_pre * 10_000 + amount_in_with_fee);

    assert!(expected_out > 0, "expected swap output must be positive");

    mint(&env, &token_0_asset, &swapper, swap_amount_in);
    token_0.transfer(&swapper, &pair_addr, &swap_amount_in);

    let swapper_token_1_before = token_1.balance(&swapper);

    // swap returns Result<(), PairError>
    pair.try_swap(&0, &expected_out, &swapper).unwrap().unwrap();

    let swapper_token_1_after = token_1.balance(&swapper);
    assert_eq!(
        swapper_token_1_after - swapper_token_1_before,
        expected_out,
        "swapper received wrong amount of token_1"
    );

    let (reserve_a_post, reserve_b_post, _) = pair.try_get_reserves().unwrap().unwrap();
    assert!(reserve_a_post > reserve_a_pre, "reserve_a must increase after token_0 input");
    assert!(reserve_b_post < reserve_b_pre, "reserve_b must decrease after token_1 output");

    let k_pre = reserve_a_pre * reserve_b_pre;
    let k_post = reserve_a_post * reserve_b_post;
    assert!(k_post >= k_pre, "K invariant must not decrease after swap");

    // ── 8. Reverse swap: token_1 in, token_0 out ─────────────────
    let swap_amount_in_rev: i128 = 200_000_000;

    let (reserve_a_rev, reserve_b_rev, _) = pair.try_get_reserves().unwrap().unwrap();
    let fee_bps_rev = pair.get_current_fee_bps() as i128;

    let amount_in_fee_rev = swap_amount_in_rev * (10_000 - fee_bps_rev);
    let expected_out_rev =
        (amount_in_fee_rev * reserve_a_rev) / (reserve_b_rev * 10_000 + amount_in_fee_rev);

    mint(&env, &token_1_asset, &swapper, swap_amount_in_rev);
    token_1.transfer(&swapper, &pair_addr, &swap_amount_in_rev);

    pair.try_swap(&expected_out_rev, &0, &swapper).unwrap().unwrap();

    let (reserve_a_post_rev, reserve_b_post_rev, _) = pair.try_get_reserves().unwrap().unwrap();
    assert!(reserve_b_post_rev > reserve_b_post, "reserve_b must increase after token_1 input");
    assert!(reserve_a_post_rev < reserve_a_post, "reserve_a must decrease after token_0 output");

    // ── 9. Burn LP ────────────────────────────────────────────────
    let lp_balance_before_burn = lp_token.balance(&lp_provider);
    assert!(lp_balance_before_burn > 0, "LP provider must hold LP tokens before burn");

    let (reserve_a_burn, reserve_b_burn, _) = pair.try_get_reserves().unwrap().unwrap();
    let total_supply_before_burn = lp_token.total_supply();

    let expected_return_0 =
        lp_balance_before_burn * reserve_a_burn / total_supply_before_burn;
    let expected_return_1 =
        lp_balance_before_burn * reserve_b_burn / total_supply_before_burn;

    let provider_token_0_before = token_0.balance(&lp_provider);
    let provider_token_1_before = token_1.balance(&lp_provider);

    // Transfer LP tokens into the pair contract, then call burn
    lp_token.transfer(&lp_provider, &pair_addr, &lp_balance_before_burn);

    // burn returns Result<(i128, i128), PairError>
    let (returned_0, returned_1) = pair.try_burn(&lp_provider).unwrap().unwrap();

    assert!(returned_0 > 0, "burn must return token_0");
    assert!(returned_1 > 0, "burn must return token_1");

    assert!(
        (returned_0 - expected_return_0).abs() <= 1,
        "returned token_0 mismatch: got {returned_0}, expected {expected_return_0}"
    );
    assert!(
        (returned_1 - expected_return_1).abs() <= 1,
        "returned token_1 mismatch: got {returned_1}, expected {expected_return_1}"
    );

    assert_eq!(
        token_0.balance(&lp_provider),
        provider_token_0_before + returned_0,
        "provider token_0 balance mismatch after burn"
    );
    assert_eq!(
        token_1.balance(&lp_provider),
        provider_token_1_before + returned_1,
        "provider token_1 balance mismatch after burn"
    );

    let total_supply_after_burn = lp_token.total_supply();
    assert_eq!(
        total_supply_before_burn - lp_balance_before_burn,
        total_supply_after_burn,
        "total LP supply must decrease by burned amount"
    );
    assert_eq!(
        lp_token.balance(&lp_provider),
        0,
        "LP provider should hold 0 LP tokens after full burn"
    );

    let (reserve_a_final, reserve_b_final, _) = pair.try_get_reserves().unwrap().unwrap();
    assert!(reserve_a_final < reserve_a_burn, "reserve_a must decrease after burn");
    assert!(reserve_b_final < reserve_b_burn, "reserve_b must decrease after burn");

    // ── 10. Pair still registered after full lifecycle ────────────
    assert_eq!(
        factory.get_pair(&token_0_id, &token_1_id),
        Some(pair_addr.clone()),
        "pair must still be registered after full lifecycle"
    );
    assert_eq!(factory.get_pair_count(), 1, "pair count must remain 1");
}

// ----------------------------------------------------------------
// Edge case: identical tokens are rejected
// ----------------------------------------------------------------

#[test]
fn test_create_pair_identical_tokens_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract(admin.clone());

    let pair_hash = env.deployer().upload_contract_wasm(pair_wasm::WASM);
    let lp_hash = env.deployer().upload_contract_wasm(lp_wasm::WASM);

    let (_, factory) = deploy_factory(&env, pair_hash, lp_hash, &admin);

    assert!(
        factory.try_create_pair(&token_id, &token_id).unwrap().is_err(),
        "identical tokens must be rejected"
    );
}

// ----------------------------------------------------------------
// Edge case: paused factory rejects create_pair
// ----------------------------------------------------------------

#[test]
fn test_create_pair_when_paused_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_a_id = env.register_stellar_asset_contract(admin.clone());
    let token_b_id = env.register_stellar_asset_contract(admin.clone());

    let pair_hash = env.deployer().upload_contract_wasm(pair_wasm::WASM);
    let lp_hash = env.deployer().upload_contract_wasm(lp_wasm::WASM);

    let (_, factory) = deploy_factory(&env, pair_hash, lp_hash, &admin);

    let signers = Vec::from_array(&env, [admin.clone()]);
    // pause returns Result<(), FactoryError>
    factory.try_pause(&signers).unwrap().unwrap();
    assert!(factory.is_paused(), "factory must be paused");

    assert!(
        factory.try_create_pair(&token_a_id, &token_b_id).unwrap().is_err(),
        "create_pair must fail when factory is paused"
    );
}

// ----------------------------------------------------------------
// Edge case: LP supply tracks correctly for token_1-dominant entry
// ----------------------------------------------------------------

#[test]
fn test_lp_supply_matches_expected_value_throughout() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let lp_provider = Address::generate(&env);

    let token_a_id = env.register_stellar_asset_contract(admin.clone());
    let token_b_id = env.register_stellar_asset_contract(admin.clone());

    let (token_0_id, token_1_id) = if token_a_id < token_b_id {
        (token_a_id.clone(), token_b_id.clone())
    } else {
        (token_b_id.clone(), token_a_id.clone())
    };

    let token_0_asset = StellarAssetClient::new(&env, &token_0_id);
    let token_1_asset = StellarAssetClient::new(&env, &token_1_id);
    let token_0 = TokenClient::new(&env, &token_0_id);
    let token_1 = TokenClient::new(&env, &token_1_id);

    let pair_hash = env.deployer().upload_contract_wasm(pair_wasm::WASM);
    let lp_hash = env.deployer().upload_contract_wasm(lp_wasm::WASM);

    let (_, factory) = deploy_factory(&env, pair_hash, lp_hash, &admin);
    let pair_addr = factory
        .try_create_pair(&token_0_id, &token_1_id)
        .unwrap()
        .unwrap();

    let pair = pair_wasm::PairClient::new(&env, &pair_addr);

    // token_1-dominant initial deposit
    let deposit_0: i128 = 500_000_000;
    let deposit_1: i128 = 2_000_000_000;

    mint(&env, &token_0_asset, &lp_provider, deposit_0);
    mint(&env, &token_1_asset, &lp_provider, deposit_1);

    token_0.transfer(&lp_provider, &pair_addr, &deposit_0);
    token_1.transfer(&lp_provider, &pair_addr, &deposit_1);

    let lp_minted = pair.try_mint(&lp_provider).unwrap().unwrap();

    let lp_token_addr = pair.try_lp_token().unwrap().unwrap();
    let lp_token = lp_wasm::LpClient::new(&env, &lp_token_addr);

    let total_supply = lp_token.total_supply();
    let provider_balance = lp_token.balance(&lp_provider);

    // geometric mean: sqrt(deposit_0 * deposit_1) - MINIMUM_LIQUIDITY (1000)
    let expected_liquidity =
        ((deposit_0 as f64 * deposit_1 as f64).sqrt() as i128) - 1000;

    assert!(
        (lp_minted - expected_liquidity).abs() <= 2,
        "LP minted {lp_minted} should be close to geometric mean {expected_liquidity}"
    );
    assert_eq!(provider_balance, lp_minted, "provider balance must equal minted");
    assert_eq!(
        total_supply,
        lp_minted + 1000,
        "total supply = minted + MINIMUM_LIQUIDITY"
    );
}