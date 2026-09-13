#![cfg(test)]

use super::*;
use asp_membership::{ASPMembership, ASPMembershipClient};
use soroban_sdk::{
    Env,
    testutils::{Address as _, Ledger as _},
    token::TokenClient,
};

const LEVELS: u32 = 16;
const COOLDOWN: u32 = 100;

struct Setup {
    env: Env,
    admin: Address,
    asp: ASPMembershipClient<'static>,
    faucet: FaucetClient<'static>,
    token: Option<Address>,
}

/// Deploy an allowlist and a faucet, and hand the allowlist admin role over so
/// the faucet can enrol on a caller's behalf.
fn setup(with_token: bool) -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token = with_token.then(|| {
        env.register_stellar_asset_contract_v2(admin.clone())
            .address()
    });

    let asp_id = env.register(ASPMembership, (admin.clone(), LEVELS));
    let asp = ASPMembershipClient::new(&env, &asp_id);

    let faucet_id = env.register(
        Faucet,
        (
            admin.clone(),
            token.clone(),
            asp_id.clone(),
            100_i128,
            COOLDOWN,
        ),
    );
    let faucet = FaucetClient::new(&env, &faucet_id);

    asp.update_admin(&faucet_id);
    if let Some(token) = &token {
        soroban_sdk::token::StellarAssetClient::new(&env, token).set_admin(&faucet_id);
    }

    Setup {
        env,
        admin,
        asp,
        faucet,
        token,
    }
}

#[test]
fn onboard_enrols_the_caller_and_moves_the_allowlist_root() {
    let s = setup(false);
    let tester = Address::generate(&s.env);
    let before = s.asp.get_root();

    s.faucet.onboard(&tester, &U256::from_u32(&s.env, 0xA11CE));

    assert_ne!(s.asp.get_root(), before);
}

#[test]
fn onboard_also_mints_when_a_token_is_configured() {
    let s = setup(true);
    let tester = Address::generate(&s.env);
    let token = TokenClient::new(&s.env, s.token.as_ref().unwrap_or_else(|| unreachable!()));

    s.faucet.onboard(&tester, &U256::from_u32(&s.env, 0xB0B));

    assert_eq!(token.balance(&tester), 100);
}

#[test]
fn a_faucet_without_a_token_enrols_but_refuses_to_drip() {
    let s = setup(false);
    let tester = Address::generate(&s.env);

    s.faucet.onboard(&tester, &U256::from_u32(&s.env, 0xC0FFEE));
    assert_eq!(
        s.faucet.try_drip(&tester),
        Err(Ok(Error::NoTokenConfigured))
    );
}

#[test]
fn the_cooldown_holds_a_second_call_and_releases_it_later() {
    let s = setup(true);
    let tester = Address::generate(&s.env);
    let token = TokenClient::new(&s.env, s.token.as_ref().unwrap_or_else(|| unreachable!()));

    s.faucet.onboard(&tester, &U256::from_u32(&s.env, 0x01));
    assert_eq!(
        s.faucet.try_drip(&tester),
        Err(Ok(Error::CooldownActive)),
        "a second call inside the cooldown window must be refused"
    );
    assert_eq!(token.balance(&tester), 100);

    s.env
        .ledger()
        .set_sequence_number(s.env.ledger().sequence() + COOLDOWN);
    s.faucet.drip(&tester);

    assert_eq!(token.balance(&tester), 200);
}

/// The cooldown is per recipient, so one tester cannot block another.
#[test]
fn the_cooldown_is_per_recipient() {
    let s = setup(false);
    let first = Address::generate(&s.env);
    let second = Address::generate(&s.env);

    s.faucet.onboard(&first, &U256::from_u32(&s.env, 0x11));
    s.faucet.onboard(&second, &U256::from_u32(&s.env, 0x22));

    assert_eq!(
        s.faucet.try_onboard(&first, &U256::from_u32(&s.env, 0x33)),
        Err(Ok(Error::CooldownActive))
    );
}

#[test]
fn settings_are_readable_and_admin_gated() {
    let s = setup(true);
    assert_eq!(s.faucet.drip_amount(), 100);
    assert_eq!(s.faucet.cooldown_ledgers(), COOLDOWN);

    s.faucet.set_drip(&250, &7);
    assert_eq!(s.faucet.drip_amount(), 250);
    assert_eq!(s.faucet.cooldown_ledgers(), 7);

    s.faucet.update_admin(&Address::generate(&s.env));
    assert_eq!(s.faucet.drip_amount(), 250);

    s.env.set_auths(&[]);
    assert!(s.faucet.try_set_drip(&500, &7).is_err());
    assert!(s.faucet.try_update_admin(&s.admin).is_err());
}

#[test]
fn a_non_positive_drip_is_refused() {
    let s = setup(true);
    assert_eq!(s.faucet.try_set_drip(&0, &7), Err(Ok(Error::InvalidAmount)));
}
