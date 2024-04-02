use plonky2::{
    iop::{
        target::Target,
        witness::{PartialWitness, WitnessWrite},
    },
    plonk::{circuit_builder::CircuitBuilder, circuit_data::CircuitConfig},
};

use crate::{
    block_finality::validator_set_hash::ValidatorSetHash,
    block_finality::BlockFinality,
    common::{
        array_to_bits,
        targets::{
            impl_target_set, ArrayTarget, BitArrayTarget, Blake2Target, Blake2TargetGoldilocks,
            Ed25519PublicKeyTarget, PaddedValidatorSetTarget, TargetBitOperations, TargetSet,
        },
        BuilderExt,
    },
    consts::MAX_VALIDATOR_COUNT,
    prelude::*,
    storage_inclusion::StorageInclusion,
    ProofWithCircuitData,
};

// record for each validator: (AccountId, SessionKeys)
// SessionKeys = (Babe, Grandpa, ImOnline, AuthorityDiscovery)
const SESSION_KEYS_SIZE: usize = 5 * 32;

impl_target_set! {
    pub struct NextValidatorSetTarget {
        pub validator_set_hash: Blake2TargetGoldilocks,
        pub next_validator_set_hash: Blake2TargetGoldilocks,
        pub current_authority_set_id: Target,
    }
}

pub struct NextValidatorSet {
    pub current_epoch_block_finality: BlockFinality,
    pub next_validator_set_inclusion_proof: StorageInclusion,
    pub next_validator_set_storage_data: Vec<u8>,
}

impl NextValidatorSet {
    pub fn prove(&self) -> ProofWithCircuitData<NextValidatorSetTarget> {
        log::info!("Proving validator set hash change...");

        let mut next_validator_set = vec![];
        // TODO Will be gone when pallet-gear-bridges will be implemented.
        for validator_idx in 0..MAX_VALIDATOR_COUNT {
            next_validator_set.push(
                self.next_validator_set_storage_data[1
                    + validator_idx * SESSION_KEYS_SIZE
                    + consts::ED25519_PUBLIC_KEY_SIZE * 2
                    ..1 + validator_idx * SESSION_KEYS_SIZE + consts::ED25519_PUBLIC_KEY_SIZE * 3]
                    .try_into()
                    .unwrap(),
            );
        }

        // TODO: Remove when pallet-gear-bridges will be implemented.
        let validator_set_hash_proof = ValidatorSetHash {
            validator_set: next_validator_set.try_into().unwrap(),
        }
        .prove();

        let non_hashed_next_validator_set_proof = NextValidatorSetNonHashed {
            current_epoch_block_finality: self.current_epoch_block_finality.clone(),
            next_validator_set_inclusion_proof: self.next_validator_set_inclusion_proof.clone(),
            next_validator_set_storage_data: self.next_validator_set_storage_data.clone(),
        }
        .prove();

        let mut builder = CircuitBuilder::new(CircuitConfig::standard_recursion_config());
        let mut witness = PartialWitness::new();

        let validator_set_hash_target =
            builder.recursively_verify_constant_proof(&validator_set_hash_proof, &mut witness);
        let next_validator_set_target = builder
            .recursively_verify_constant_proof(&non_hashed_next_validator_set_proof, &mut witness);

        validator_set_hash_target
            .validator_set
            .connect(&next_validator_set_target.next_validator_set, &mut builder);

        NextValidatorSetTarget {
            validator_set_hash: Blake2TargetGoldilocks::from_blake2_target(
                next_validator_set_target.current_validator_set_hash,
                &mut builder,
            ),
            next_validator_set_hash: Blake2TargetGoldilocks::from_blake2_target(
                validator_set_hash_target.hash,
                &mut builder,
            ),
            current_authority_set_id: next_validator_set_target.authority_set_id,
        }
        .register_as_public_inputs(&mut builder);

        ProofWithCircuitData::from_builder(builder, witness)
    }
}

impl_target_set! {
    struct NextValidatorSetNonHashedTarget {
        current_validator_set_hash: Blake2Target,
        authority_set_id: Target,
        next_validator_set: PaddedValidatorSetTarget,
    }
}

impl_target_set! {
    struct SessionKeysTarget {
        _session_key: Ed25519PublicKeyTarget,
        _babe_key: Ed25519PublicKeyTarget,
        pub grandpa_key: Ed25519PublicKeyTarget,
        _imonline_key: Ed25519PublicKeyTarget,
        _authoryty_discovery_key: Ed25519PublicKeyTarget,
    }
}

// TODO: Will be gone when pallet-gear-bridges get implemented.
impl_target_set! {
    struct ValidatorSetInStorageTarget {
        _length: BitArrayTarget<8>,
        validators: ArrayTarget<SessionKeysTarget, MAX_VALIDATOR_COUNT>,
    }
}

impl ValidatorSetInStorageTarget {
    fn into_grandpa_authority_keys(self) -> PaddedValidatorSetTarget {
        PaddedValidatorSetTarget::parse(
            &mut self
                .validators
                .0
                .into_iter()
                .flat_map(|v| v.grandpa_key.into_targets_iter()),
        )
    }
}

struct NextValidatorSetNonHashed {
    current_epoch_block_finality: BlockFinality,
    next_validator_set_inclusion_proof: StorageInclusion,
    next_validator_set_storage_data: Vec<u8>,
}

impl NextValidatorSetNonHashed {
    pub fn prove(self) -> ProofWithCircuitData<NextValidatorSetNonHashedTarget> {
        log::info!("Proving validator set change...");

        let next_validator_set_bits = array_to_bits(&self.next_validator_set_storage_data);

        let inclusion_proof = self.next_validator_set_inclusion_proof.prove();
        let block_finality_proof = self.current_epoch_block_finality.prove();

        let config = CircuitConfig::standard_recursion_config();
        let mut builder = CircuitBuilder::new(config);
        let mut witness = PartialWitness::new();

        let inclusion_proof_target =
            builder.recursively_verify_constant_proof(&inclusion_proof, &mut witness);
        let block_finality_target =
            builder.recursively_verify_constant_proof(&block_finality_proof, &mut witness);

        inclusion_proof_target
            .block_hash
            .connect(&block_finality_target.message.block_hash, &mut builder);

        let authority_set_id = Target::from_u64_bits_le_lossy(
            block_finality_target.message.authority_set_id,
            &mut builder,
        );

        let next_validator_set_targets: Vec<_> = next_validator_set_bits
            .into_iter()
            .map(|bit| {
                let target = builder.add_virtual_bool_target_safe();
                witness.set_bool_target(target, bit);
                target
            })
            .collect();

        let next_validator_set_hash = plonky2_blake2b256::circuit::blake2_circuit_from_targets(
            &mut builder,
            next_validator_set_targets.clone(),
        );
        let next_validator_set_hash =
            Blake2Target::parse_exact(&mut next_validator_set_hash.into_iter().map(|t| t.target));

        next_validator_set_hash.connect(&inclusion_proof_target.storage_item_hash, &mut builder);

        let next_validator_set = ValidatorSetInStorageTarget::parse_exact(
            &mut next_validator_set_targets.into_iter().map(|t| t.target),
        )
        .into_grandpa_authority_keys();

        NextValidatorSetNonHashedTarget {
            current_validator_set_hash: block_finality_target.validator_set_hash,
            authority_set_id,
            next_validator_set,
        }
        .register_as_public_inputs(&mut builder);

        ProofWithCircuitData::from_builder(builder, witness)
    }
}
