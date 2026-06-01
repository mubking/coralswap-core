use soroban_sdk::{contracttype, Address, Env, Vec};

#[contracttype]
pub enum DataKey {
    Factory,
    Hubs,
}

pub fn set_factory(env: &Env, factory: &Address) {
    env.storage().instance().set(&DataKey::Factory, factory);
}

pub fn get_factory(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::Factory)
}

pub fn set_hubs(env: &Env, hubs: &Vec<Address>) {
    env.storage().instance().set(&DataKey::Hubs, hubs);
}

pub fn get_hubs(env: &Env) -> Vec<Address> {
    env.storage().instance().get(&DataKey::Hubs).unwrap_or(Vec::new(env))
}
