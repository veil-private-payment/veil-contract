//! Testnet onboarding faucet.
//!
//! A spender cannot move a note unless their note key sits in the ASP
//! membership tree, and that tree only accepts writes from its admin. On a
//! shared testnet that leaves every new tester waiting on a human. This
//! contract holds the admin role instead and hands it out under a per-address
//! cooldown, so a tester can onboard themselves with no backend and no shared
//! key.
//!
//! Two shapes are supported:
//!
//! - **Enrolment only**, when the pool settles a token the faucet cannot mint,
//!   such as the native asset. Testers fund themselves from friendbot and the
//!   faucet only enrols them.
//! - **Enrolment and mint**, when the pool settles an asset whose Stellar Asset
//!   Contract has the faucet as its admin. `onboard` then also mints the drip.
//!
//! This is a testnet convenience. It deliberately grants allowlist admission to
//! anyone who asks, which is not a compliance posture for real value.
#![no_std]
use soroban_sdk::{
    Address, Env, IntoVal, Symbol, U256, contract, contracterror, contractevent, contractimpl,
    contracttype, token::StellarAssetClient, vec,
};

/// Persistent storage keys.
#[contracttype]
#[derive(Clone)]
enum DataKey {
    /// Address allowed to reconfigure the faucet.
    Admin,
    /// Stellar Asset Contract the faucet mints. Absent when the faucet only enrols.
    Token,
    /// ASP membership contract the faucet enrols into. The faucet must be its admin.
    Asp,
    /// Amount minted per call, in token base units.
    DripAmount,
    /// Minimum ledgers between calls for one recipient.
    CooldownLedgers,
    /// Ledger of the last call for a recipient.
    LastDrip(Address),
}

/// Contract error types.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// The contract has not been initialized.
    NotInitialized = 1,
    /// The recipient's cooldown has not elapsed.
    CooldownActive = 2,
    /// Configured drip amount is not positive.
    InvalidAmount = 3,
    /// The faucet was deployed without a token, so it cannot mint.
    NoTokenConfigured = 4,
}

/// Emitted when a recipient is onboarded.
#[contractevent(topics = ["Onboard"])]
struct OnboardEvent {
    /// Onboarded recipient.
    to: Address,
    /// Amount minted, zero when the faucet only enrols.
    amount: i128,
    /// Membership leaf added to the allowlist.
    leaf: U256,
}

#[contract]
pub struct Faucet;

#[contractimpl]
impl Faucet {
    /// Initialize the faucet.
    ///
    /// # Arguments
    ///
    /// * `admin` - may change the drip settings or hand on the admin role
    /// * `token` - Stellar Asset Contract to mint, or `None` to only enrol
    /// * `asp` - ASP membership contract; the faucet must be made its admin
    /// * `drip_amount` - base units minted per call, required when `token` is set
    /// * `cooldown_ledgers` - minimum ledgers between calls for one recipient
    pub fn __constructor(
        env: Env,
        admin: Address,
        token: Option<Address>,
        asp: Address,
        drip_amount: i128,
        cooldown_ledgers: u32,
    ) -> Result<(), Error> {
        if token.is_some() && drip_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let store = env.storage().persistent();
        store.set(&DataKey::Admin, &admin);
        if let Some(token) = token {
            store.set(&DataKey::Token, &token);
        }
        store.set(&DataKey::Asp, &asp);
        store.set(&DataKey::DripAmount, &drip_amount);
        store.set(&DataKey::CooldownLedgers, &cooldown_ledgers);
        Ok(())
    }

    /// Enrol `membership_leaf` in the allowlist, and mint the drip to `to` when
    /// a token is configured. Subject to `to`'s cooldown.
    ///
    /// `membership_leaf` is `Poseidon2([notePublicKey, 0], 0x01)`, computed by
    /// the client. It is a public value, and a junk leaf is inert: it only
    /// helps a spender whose proof already uses that exact leaf.
    ///
    /// The enrolment is a call into the ASP membership contract, which requires
    /// its admin's authorization. The faucet's own call authorizes it once the
    /// admin role has been handed over. A failed enrolment reverts the mint
    /// with it.
    pub fn onboard(env: Env, to: Address, membership_leaf: U256) -> Result<(), Error> {
        let amount = Self::drip_with_cooldown(&env, &to)?;

        let store = env.storage().persistent();
        let asp: Address = store.get(&DataKey::Asp).ok_or(Error::NotInitialized)?;
        env.invoke_contract::<()>(
            &asp,
            &Symbol::new(&env, "insert_leaf"),
            vec![&env, membership_leaf.clone().into_val(&env)],
        );

        OnboardEvent {
            to,
            amount,
            leaf: membership_leaf,
        }
        .publish(&env);
        Ok(())
    }

    /// Mint the drip to `to` without enrolling, subject to its cooldown.
    ///
    /// Fails when the faucet was deployed without a token.
    pub fn drip(env: Env, to: Address) -> Result<(), Error> {
        if !env.storage().persistent().has(&DataKey::Token) {
            return Err(Error::NoTokenConfigured);
        }
        Self::drip_with_cooldown(&env, &to)?;
        Ok(())
    }

    /// Enforce the cooldown, then mint if a token is configured.
    ///
    /// Returns the amount minted, which is zero for an enrolment-only faucet.
    fn drip_with_cooldown(env: &Env, to: &Address) -> Result<i128, Error> {
        let store = env.storage().persistent();
        let cooldown: u32 = store
            .get(&DataKey::CooldownLedgers)
            .ok_or(Error::NotInitialized)?;

        let now = env.ledger().sequence();
        if let Some(last) = store.get::<_, u32>(&DataKey::LastDrip(to.clone()))
            && now < last.saturating_add(cooldown)
        {
            return Err(Error::CooldownActive);
        }
        store.set(&DataKey::LastDrip(to.clone()), &now);

        let Some(token) = store.get::<_, Address>(&DataKey::Token) else {
            return Ok(0);
        };
        let amount: i128 = store
            .get(&DataKey::DripAmount)
            .ok_or(Error::NotInitialized)?;
        StellarAssetClient::new(env, &token).mint(to, &amount);
        Ok(amount)
    }

    /// Admin: update the drip amount and the cooldown.
    pub fn set_drip(env: Env, drip_amount: i128, cooldown_ledgers: u32) -> Result<(), Error> {
        if drip_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let store = env.storage().persistent();
        let admin: Address = store.get(&DataKey::Admin).ok_or(Error::NotInitialized)?;
        admin.require_auth();
        store.set(&DataKey::DripAmount, &drip_amount);
        store.set(&DataKey::CooldownLedgers, &cooldown_ledgers);
        Ok(())
    }

    /// Admin: hand the faucet's admin role to a new address.
    pub fn update_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        let store = env.storage().persistent();
        let admin: Address = store.get(&DataKey::Admin).ok_or(Error::NotInitialized)?;
        admin.require_auth();
        store.set(&DataKey::Admin, &new_admin);
        Ok(())
    }

    /// Read: the configured drip amount.
    pub fn drip_amount(env: Env) -> Result<i128, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::DripAmount)
            .ok_or(Error::NotInitialized)
    }

    /// Read: the configured cooldown, in ledgers.
    pub fn cooldown_ledgers(env: Env) -> Result<u32, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::CooldownLedgers)
            .ok_or(Error::NotInitialized)
    }
}

mod test;
