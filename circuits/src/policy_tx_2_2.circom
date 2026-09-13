pragma circom 2.2.2;
// Entry Point PolicyTransaction with 2 inputs, 2 outputs.
include "./policyTransaction.circom";

// PolicyTransaction(
//   nIns, nOuts,
//   nMembershipProofs,
//   levels
// )
// Tree depth 16 holds 65,536 leaves. The pool inserts two leaves per
// deposit and two per shielded transaction, so that is 32,768 operations.
// Depth 16 is the deepest tree the pool constructor can create: it writes two
// ledger entries per level, and a Soroban transaction may write only 50.
component main {public [root, publicAmount, extDataHash, inputNullifier, outputCommitment, membershipRoots]} = PolicyTransaction(2, 2, 1, 16);
