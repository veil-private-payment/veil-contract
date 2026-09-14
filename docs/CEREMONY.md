# Trusted setup

A Groth16 proving key is built from secret randomness. Whoever holds it can
forge a proof for any statement, which here means minting notes out of nothing.
The randomness has to be destroyed, and the only way anyone else can believe
that is a ceremony where several people each add their own and destroy it: the
key is sound as long as **one** contributor was honest.

## Where this stands

Phase 1 (Powers of Tau) and phase 2 (this circuit) were both run **by one
party**, on one machine, because the public Hermez transcript was unreachable
at the time. So today's key rests on trusting that single party. That is
acceptable for a testnet with faucet money and unacceptable for anything else.

The scripts below run the multi-party version. What they cannot supply is the
part that matters: independent people.

## Running a contribution

Each participant receives the previous participant's `.zkey`, runs:

```sh
scripts/ceremony-contribute.sh in.zkey out.zkey "your name"
```

and sends only `out.zkey` onward. The script appends a line to
`circuit-keys/ceremony-transcript.md` with the SHA-256 of the file it produced,
and prints the same hash.

Three rules, and the ceremony is worthless without them:

- **Publish your hash yourself**, somewhere tied to your name. A transcript
  written entirely by the organiser proves nothing.
- **Never send your entropy anywhere.** The script takes it from
  `CEREMONY_ENTROPY` or the machine's randomness and drops it on exit.
- **Do not keep it.** The security of the whole key is that at least one
  participant's randomness no longer exists.

Participants should be independent: different people, different machines,
ideally different organisations. Ten contributions from one laptop are one
contribution.

## Closing it

The chain ends with a public beacon, so nobody can grind their own contribution
against a known final state. Use a value that did not exist when the ceremony
started and that nobody controls — a future Stellar ledger hash works, as phase
1 already did with ledger 64411842.

```sh
npx snarkjs@0.7.6 zkey beacon last.zkey final.zkey <beacon-hash> 10 -n="final beacon"
```

## Checking it

Anyone can verify, without trusting the organiser:

```sh
scripts/ceremony-verify.sh circuit-keys/policy_tx_2_2_proving_key.zkey
```

It checks the Powers of Tau transcript, walks every phase 2 contribution
against the circuit's own r1cs, and prints the hash to compare with the last
line of the transcript. Each participant should find their own line and
confirm it matches what they saw.

The verifier contract embeds the verifying key at build time, so the key a
deployment actually uses is whatever was compiled into it — check that too,
with `deployments/testnet.json` and the verifier's own build.
