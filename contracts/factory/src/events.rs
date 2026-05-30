use soroban_sdk::{Address, Env};

#[allow(dead_code)]
pub struct FactoryEvents;

#[allow(dead_code)]
impl FactoryEvents {
    pub fn pair_created(
        env: &Env,
        token_a: &Address,
        token_b: &Address,
        pair: &Address,
        pair_index: u32,
    ) {
        let topics = (soroban_sdk::symbol_short!("created"), token_a.clone(), token_b.clone());
        env.events().publish(topics, (pair.clone(), pair_index));
    }

    pub fn paused(env: &Env) {
        env.events().publish((soroban_sdk::symbol_short!("paused"),), ());
    }

    pub fn unpaused(env: &Env) {
        env.events().publish((soroban_sdk::symbol_short!("unpaused"),), ());
    }

    pub fn upgrade_proposed(env: &Env, new_wasm_hash: &[u8; 32]) {
        env.events().publish(
            (soroban_sdk::symbol_short!("prop_upg"),),
            soroban_sdk::BytesN::from_array(env, new_wasm_hash),
        );
    }

    pub fn upgrade_executed(env: &Env, new_version: u32) {
        env.events().publish((soroban_sdk::symbol_short!("upgraded"),), new_version);
    }

    pub fn fee_to_set(env: &Env, new_fee_to: &Option<Address>) {
        env.events().publish((soroban_sdk::symbol_short!("fee_to"),), new_fee_to.clone());
    }

    pub fn fee_to_setter_set(env: &Env, new_setter: &Address) {
        env.events().publish((soroban_sdk::symbol_short!("setter"),), new_setter.clone());
    }

    pub fn protocol_fee_updated(env: &Env, old_fee_bps: u32, new_fee_bps: u32, fee_to: &Option<Address>) {
        env.events().publish(
            (soroban_sdk::symbol_short!("fee_upd"),),
            (old_fee_bps, new_fee_bps, fee_to.clone()),
        );
    }

    pub fn protocol_fees_collected(
        env: &Env,
        pair: &Address,
        amount_0: i128,
        amount_1: i128,
        ledger: u32,
    ) {
        env.events().publish(
            (soroban_sdk::symbol_short!("proto_cl"), pair.clone()),
            (amount_0, amount_1, ledger),
        );
    }
}
