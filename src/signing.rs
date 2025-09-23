use aws_sigv4::http_request::{
    sign, SignableBody, SignableRequest, SigningParams as HttpSigningParams, SigningSettings,
};
use aws_sigv4::sign::v4::SigningParams;
use http::Request;
use std::time::SystemTime;

use crate::state::AppState;

pub fn sign_request(
    req: Request<Vec<u8>>,
    state: &AppState,
    is_streaming: bool,
) -> Result<reqwest::Request, Box<dyn std::error::Error>> {
    let identity = aws_credential_types::Credentials::new(
        &state.access_key,
        &state.secret_key,
        state.session_token.clone(),
        None,
        "hardcoded-credentials",
    )
    .into();

    let signing_settings = SigningSettings::default();

    let signing_params: HttpSigningParams = SigningParams::builder()
        .identity(&identity)
        .region(&state.region)
        .name("bedrock")
        .time(SystemTime::now())
        .settings(signing_settings)
        .build()
        .unwrap()
        .into();

    let host_header = format!("bedrock-runtime.{}.amazonaws.com", state.region);

    let mut headers = vec![
        ("content-type", "application/json"),
        ("host", host_header.as_str()),
    ];

    let accept_header = if is_streaming {
        "application/vnd.amazon.eventstream"
    } else {
        "application/json"
    };
    headers.push(("accept", accept_header));

    let signable_req = SignableRequest::new(
        req.method().as_str(),
        req.uri().to_string(),
        headers.clone().into_iter(),
        SignableBody::Bytes(req.body()),
    )?;

    let (signing_instructions, _signature) = sign(signable_req, &signing_params)?.into_parts();

    let mut signed_http = http::Request::builder()
        .method(req.method().clone())
        .uri(req.uri().clone())
        .header("content-type", "application/json")
        .header("accept", accept_header)
        .body(req.body().clone())?;

    signing_instructions.apply_to_request_http1x(&mut signed_http);

    let mut builder = state.client.request(
        reqwest::Method::from_bytes(req.method().as_str().as_bytes())?,
        req.uri().to_string(),
    );

    for (k, v) in signed_http.headers().iter() {
        builder = builder.header(k, v);
    }

    builder = builder.body(signed_http.body().clone());

    Ok(builder.build()?)
}