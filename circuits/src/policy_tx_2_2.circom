pragma circom 2.2.2;
// Entry Point PolicyTransaction with 2 inputs, 2 outputs.
include "./policyTransaction.circom";

// PolicyTransaction(
//   nIns, nOuts,
//   nMembershipProofs,
//   levels
// )
component main {public [root, publicAmount, extDataHash, inputNullifier, outputCommitment, membershipRoots]} = PolicyTransaction(2, 2, 1, 10);
