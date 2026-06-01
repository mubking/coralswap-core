use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, testutils::Address as _, Address, Env,
    Vec,
};

// ---------------------------------------------------------------------------
// MockFactory
// ---------------------------------------------------------------------------

#[contract]
pub struct MockFactory;

#[contracttype]
#[derive(Clone)]
pub enum MFKey {
    Pair(Address, Address),
}

#[contractimpl]
impl MockFactory {
    pub fn set_pair(env: Env, token_a: Address, token_b: Address, pair: Address) {
        let (t0, t1) = if token_a < token_b { (token_a, token_b) } else { (token_b, token_a) };
        env.storage().instance().set(&MFKey::Pair(t0, t1), &pair);
    }

    pub fn get_pair(env: Env, token_a: Address, token_b: Address) -> Option<Address> {
        let (t0, t1) = if token_a < token_b { (token_a, token_b) } else { (token_b, token_a) };
        env.storage().instance().get(&MFKey::Pair(t0, t1))
    }

    pub fn create_pair(_env: Env, _token_a: Address, _token_b: Address) -> Address {
        panic!("not needed for router unit tests")
    }
}

// ---------------------------------------------------------------------------
// MockPair
// ---------------------------------------------------------------------------

#[contract]
pub struct MockPair;

#[contracttype]
#[derive(Clone)]
pub enum MPKey {
    ReserveA,
    ReserveB,
    BurnAmountA,
    BurnAmountB,
    LiquidityToMint,
}

#[contractimpl]
impl MockPair {
    pub fn set_reserves(env: Env, reserve_a: i128, reserve_b: i128) {
        env.storage().instance().set(&MPKey::ReserveA, &reserve_a);
        env.storage().instance().set(&MPKey::ReserveB, &reserve_b);
    }

    pub fn get_reserves(env: Env) -> (i128, i128, u64) {
        let a: i128 = env.storage().instance().get(&MPKey::ReserveA).unwrap_or(0);
        let b: i128 = env.storage().instance().get(&MPKey::ReserveB).unwrap_or(0);
        (a, b, 0)
    }

    pub fn set_burn_amounts(env: Env, amount_a: i128, amount_b: i128) {
        env.storage().instance().set(&MPKey::BurnAmountA, &amount_a);
        env.storage().instance().set(&MPKey::BurnAmountB, &amount_b);
    }

    pub fn burn(env: Env, _to: Address) -> (i128, i128) {
        let a: i128 = env.storage().instance().get(&MPKey::BurnAmountA).unwrap_or(0);
        let b: i128 = env.storage().instance().get(&MPKey::BurnAmountB).unwrap_or(0);
        (a, b)
    }

    pub fn set_liquidity_to_mint(env: Env, liquidity: i128) {
        env.storage().instance().set(&MPKey::LiquidityToMint, &liquidity);
    }

    pub fn mint(env: Env, _to: Address) -> i128 {
        env.storage().instance().get(&MPKey::LiquidityToMint).unwrap_or(0)
    }

    pub fn swap(_env: Env, _amount_a_out: i128, _amount_b_out: i128, _to: Address) {}

    pub fn lp_token(_env: Env) -> Address {
        panic!("not needed for router unit tests")
    }

    pub fn get_current_fee_bps(_env: Env) -> u32 {
        30
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

use crate::Router;

fn deploy_router(env: &Env) -> (Address, Address) {
    let router_id = env.register_contract(None, Router);
    let factory_id = env.register_contract(None, MockFactory);
    let router = RouterClient::new(env, &router_id);
    router.initialize(&factory_id, &Vec::new(env));
    (router_id, factory_id)
}

fn generate_tokens(env: &Env, n: u32) -> Vec<Address> {
    let mut tokens: Vec<Address> = Vec::new(env);
    for _ in 0..n {
        tokens.push_back(Address::generate(env));
    }
    tokens
}

fn setup_pair(
    env: &Env,
    factory_id: &Address,
    token_a: &Address,
    token_b: &Address,
    reserve_a: i128,
    reserve_b: i128,
) -> Address {
    let pair_id = env.register_contract(None, MockPair);
    let pair = MockPairClient::new(env, &pair_id);
    pair.set_reserves(&reserve_a, &reserve_b);

    let factory = MockFactoryClient::new(env, factory_id);
    factory.set_pair(token_a, token_b, &pair_id);
    pair_id
}

fn make_path(env: &Env, tokens: &Vec<Address>) -> Vec<Address> {
    let mut path: Vec<Address> = Vec::new(env);
    for i in 0..tokens.len() {
        path.push_back(tokens.get(i).unwrap());
    }
    path
}

// ---------------------------------------------------------------------------
// RouterClient helper
// ---------------------------------------------------------------------------

#[contractclient(name = "RouterClient")]
#[allow(dead_code)]
pub trait RouterInterface {
    fn initialize(env: Env, factory: Address, hubs: Vec<Address>);
    fn set_hubs(env: Env, hubs: Vec<Address>);
    fn get_hubs(env: Env) -> Vec<Address>;
    fn get_best_path(
        env: Env,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
    ) -> (Vec<Address>, i128);
    fn swap_exact_tokens_multi_hop(
        env: Env,
        path: Vec<Address>,
        amount_in: i128,
        amount_out_min: i128,
        to: Address,
        deadline: u64,
    ) -> i128;
    fn swap_exact_tokens_for_tokens(
        env: Env,
        amount_in: i128,
        amount_out_min: i128,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Vec<i128>;
    fn swap_tokens_for_exact_tokens(
        env: Env,
        amount_out: i128,
        amount_in_max: i128,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Vec<i128>;
    fn add_liquidity(
        env: Env,
        token_a: Address,
        token_b: Address,
        amount_a_desired: i128,
        amount_b_desired: i128,
        amount_a_min: i128,
        amount_b_min: i128,
        to: Address,
        deadline: u64,
    ) -> (i128, i128, i128);
    fn remove_liquidity(
        env: Env,
        token_a: Address,
        token_b: Address,
        liquidity: i128,
        amount_a_min: i128,
        amount_b_min: i128,
        to: Address,
        deadline: u64,
    ) -> (i128, i128);
}

mod helpers_test;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_contract_compiles() {
    // Contract compiles and links correctly
}

// ===================== get_best_path =====================

#[test]
fn test_get_best_path_identical_tokens() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let token = Address::generate(&env);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.get_best_path(&token, &token, &1000);
    }));
    assert!(result.is_err(), "identical tokens must fail");
}

#[test]
fn test_get_best_path_zero_amount() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let a = Address::generate(&env);
    let b = Address::generate(&env);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.get_best_path(&a, &b, &0);
    }));
    assert!(result.is_err(), "zero amount must fail");
}

#[test]
fn test_get_best_path_no_factory_set() {
    let env = Env::default();
    let router_id = env.register_contract(None, Router);
    let router = RouterClient::new(&env, &router_id);
    let a = Address::generate(&env);
    let b = Address::generate(&env);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.get_best_path(&a, &b, &1000);
    }));
    assert!(result.is_err(), "no factory must fail");
}

#[test]
fn test_get_best_path_direct_pair() {
    let env = Env::default();
    let (router_id, factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 2);
    let token_a = tokens.get(0).unwrap();
    let token_b = tokens.get(1).unwrap();

    setup_pair(&env, &factory_id, &token_a, &token_b, 100_000, 100_000);

    let (path, expected_out) = router.get_best_path(&token_a, &token_b, &1000);
    assert_eq!(path.len(), 2, "direct path must have 2 entries");
    assert_eq!(path.get(0).unwrap(), token_a);
    assert_eq!(path.get(1).unwrap(), token_b);
    assert!(expected_out > 0, "expected output must be positive");
}

#[test]
fn test_get_best_path_two_hop() {
    let env = Env::default();
    let (router_id, factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 3);
    let token_a = tokens.get(0).unwrap();
    let token_b = tokens.get(1).unwrap();
    let hub = tokens.get(2).unwrap();

    let mut hubs: Vec<Address> = Vec::new(&env);
    hubs.push_back(hub.clone());
    router.set_hubs(&hubs);

    setup_pair(&env, &factory_id, &token_a, &hub, 100_000, 100_000);
    setup_pair(&env, &factory_id, &hub, &token_b, 200_000, 200_000);

    let (path, expected_out) = router.get_best_path(&token_a, &token_b, &1000);
    assert_eq!(path.len(), 3, "2-hop path must have 3 entries");
    assert_eq!(path.get(0).unwrap(), token_a);
    assert_eq!(path.get(1).unwrap(), hub);
    assert_eq!(path.get(2).unwrap(), token_b);
    assert!(expected_out > 0, "expected output must be positive");
}

#[test]
fn test_get_best_path_prefers_highest_output() {
    let env = Env::default();
    let (router_id, factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 4);
    let token_a = tokens.get(0).unwrap();
    let token_b = tokens.get(1).unwrap();
    let hub1 = tokens.get(2).unwrap();
    let hub2 = tokens.get(3).unwrap();

    let mut hubs: Vec<Address> = Vec::new(&env);
    hubs.push_back(hub1.clone());
    hubs.push_back(hub2.clone());
    router.set_hubs(&hubs);

    // Direct pair with very low liquidity → low output
    setup_pair(&env, &factory_id, &token_a, &token_b, 500, 500);

    // hub1 route with high liquidity
    setup_pair(&env, &factory_id, &token_a, &hub1, 100_000, 100_000);
    setup_pair(&env, &factory_id, &hub1, &token_b, 100_000, 100_000);

    // hub2 route with low liquidity
    setup_pair(&env, &factory_id, &token_a, &hub2, 1_000, 1_000);
    setup_pair(&env, &factory_id, &hub2, &token_b, 1_000, 1_000);

    let (path, expected_out) = router.get_best_path(&token_a, &token_b, &1000);
    assert_eq!(path.len(), 3, "should select 2-hop via best hub");
    assert_eq!(path.get(1).unwrap(), hub1, "should prefer higher-liquidity hub");
    assert!(expected_out > 0);
}

#[test]
fn test_get_best_path_three_hop() {
    let env = Env::default();
    let (router_id, factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 4);
    let token_a = tokens.get(0).unwrap();
    let token_b = tokens.get(1).unwrap();
    let hub1 = tokens.get(2).unwrap();
    let hub2 = tokens.get(3).unwrap();

    let mut hubs: Vec<Address> = Vec::new(&env);
    hubs.push_back(hub1.clone());
    hubs.push_back(hub2.clone());
    router.set_hubs(&hubs);

    setup_pair(&env, &factory_id, &token_a, &hub1, 100_000, 100_000);
    setup_pair(&env, &factory_id, &hub1, &hub2, 100_000, 100_000);
    setup_pair(&env, &factory_id, &hub2, &token_b, 100_000, 100_000);

    let (path, expected_out) = router.get_best_path(&token_a, &token_b, &1000);
    assert_eq!(path.len(), 4, "3-hop path must have 4 entries");
    assert_eq!(path.get(0).unwrap(), token_a);
    assert_eq!(path.get(1).unwrap(), hub1);
    assert_eq!(path.get(2).unwrap(), hub2);
    assert_eq!(path.get(3).unwrap(), token_b);
    assert!(expected_out > 0);
}

#[test]
fn test_get_best_path_no_route() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 2);
    let token_a = tokens.get(0).unwrap();
    let token_b = tokens.get(1).unwrap();

    // Set up a hub but no pairs connecting token_a or token_b
    let mut hubs: Vec<Address> = Vec::new(&env);
    hubs.push_back(Address::generate(&env));
    router.set_hubs(&hubs);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.get_best_path(&token_a, &token_b, &1000);
    }));
    assert!(result.is_err(), "no feasible route must fail");
}

// ===================== swap_exact_tokens_multi_hop =====================

#[test]
fn test_swap_multi_hop_expired_deadline() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 2);
    let path = make_path(&env, &tokens);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.swap_exact_tokens_multi_hop(
            &path,
            &1000,
            &1,
            &Address::generate(&env),
            &1, // deadline in the past (ledger timestamp is 2000)
        );
    }));
    assert!(result.is_err(), "expired deadline must fail");
}

#[test]
fn test_swap_multi_hop_zero_amount() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 2);
    let path = make_path(&env, &tokens);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.swap_exact_tokens_multi_hop(&path, &0, &1, &Address::generate(&env), &u64::MAX);
    }));
    assert!(result.is_err(), "zero amount must fail");
}

#[test]
fn test_swap_multi_hop_invalid_path_too_short() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let mut path: Vec<Address> = Vec::new(&env);
    path.push_back(Address::generate(&env));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.swap_exact_tokens_multi_hop(&path, &1000, &1, &Address::generate(&env), &u64::MAX);
    }));
    assert!(result.is_err(), "too-short path must fail");
}

#[test]
fn test_swap_multi_hop_invalid_path_too_long() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 5);
    let path = make_path(&env, &tokens);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.swap_exact_tokens_multi_hop(&path, &1000, &1, &Address::generate(&env), &u64::MAX);
    }));
    assert!(result.is_err(), "too-long path (4+ hops) must fail");
}

// ===================== swap_tokens_for_exact_tokens =====================

#[test]
fn test_swap_exact_out_expired_deadline() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 2);
    let path = make_path(&env, &tokens);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.swap_tokens_for_exact_tokens(&100, &1000, &path, &Address::generate(&env), &1);
    }));
    assert!(result.is_err(), "expired deadline must fail");
}

#[test]
fn test_swap_exact_out_zero_amount() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let tokens = generate_tokens(&env, 2);
    let path = make_path(&env, &tokens);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.swap_tokens_for_exact_tokens(&0, &1000, &path, &Address::generate(&env), &u64::MAX);
    }));
    assert!(result.is_err(), "zero output amount must fail");
}

#[test]
fn test_swap_exact_out_invalid_path() {
    let env = Env::default();
    let (router_id, _factory_id) = deploy_router(&env);
    let router = RouterClient::new(&env, &router_id);
    let mut path: Vec<Address> = Vec::new(&env);
    path.push_back(Address::generate(&env));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        router.swap_tokens_for_exact_tokens(
            &100,
            &1000,
            &path,
            &Address::generate(&env),
            &u64::MAX,
        );
    }));
    assert!(result.is_err(), "too-short path must fail");
}
