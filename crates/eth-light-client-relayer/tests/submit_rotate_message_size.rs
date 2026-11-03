//! `submitRotate` carries a step proof and the committee proof in one external
//! message. The block manager accepts at most 64 KiB of request body on
//! `/v2/messages`, and tvm-sdk sends the message there as base64 inside a JSON
//! array (`ExtMessageV3`: the base64 BOC plus `id`, `thread_id`, `dapp_id`,
//! `account_id` and the block-manager token).
#![cfg(feature = "live-submit")]

use std::{path::PathBuf, sync::Arc};

use eth_light_client_relayer::{abi::light_client_abi, build_rotate_params, RotateProofBundle};
use tvm_client::{
    abi::{encode_message, CallSet, ParamsOfEncodeMessage, Signer},
    crypto::generate_random_sign_keys,
    ClientConfig, ClientContext,
};

const BM_REQUEST_BODY_LIMIT: usize = 64 * 1024;
/// Everything in the request besides the base64 body, with room to spare.
const ENVELOPE_ALLOWANCE: usize = 2 * 1024;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../eth-light-client-prover/fixtures")
}

#[tokio::test]
async fn a_signed_submit_rotate_fits_the_block_manager_request_limit() {
    let bundle = RotateProofBundle::from_dirs(
        &fixtures().join("step_vkblob"),
        &fixtures().join("rotate_vkblob"),
    )
    .expect("committed step and rotate fixtures");
    let context = Arc::new(ClientContext::new(ClientConfig::default()).unwrap());
    let keys = generate_random_sign_keys(context.clone()).unwrap();
    let encoded = encode_message(context, ParamsOfEncodeMessage {
        abi: light_client_abi(),
        address: Some(format!("0:{}", "1b".repeat(32))),
        call_set: CallSet::some_with_function_and_input(
            "submitRotate",
            build_rotate_params(&bundle),
        ),
        signer: Signer::Keys {
            keys,
        },
        ..Default::default()
    })
    .await
    .expect("submitRotate encodes against the built-in ABI");

    let payload = bundle.step_proof.len()
        + bundle.step_public_inputs.len()
        + bundle.proof.len()
        + bundle.public_inputs.len();
    let request = encoded.message.len() + ENVELOPE_ALLOWANCE;
    eprintln!(
        "submitRotate: payload {payload} B, base64 BOC {} B, request <= {request} B of \
         {BM_REQUEST_BODY_LIMIT}",
        encoded.message.len()
    );
    assert!(
        request <= BM_REQUEST_BODY_LIMIT,
        "submitRotate needs {request} B of request body, the block manager takes \
         {BM_REQUEST_BODY_LIMIT}"
    );
}
