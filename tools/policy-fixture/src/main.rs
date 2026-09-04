use anyhow::Result;

/// Regenerate the deterministic `policy_tx_2_2` fixture files under `testdata/`.
fn main() -> Result<()> {
    policy_fixture::write_fixture_files()
}
