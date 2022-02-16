//
// Copyright 2021 The Sigstore Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
#![allow(warnings, unused)]

use crate::errors::{Result, SigstoreError};

use openidconnect::core::{
    CoreClient, CoreIdTokenClaims, CoreIdTokenVerifier, CoreProviderMetadata, CoreResponseType,
};
use openidconnect::reqwest::http_client;
use openidconnect::{
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    StandardTokenResponse,
};
use std::io;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use url::Url;

#[derive(Debug, PartialEq)]
pub struct OpenID {
    pub(crate) oidc_client_id: String,
    pub(crate) oidc_client_secret: String,
    pub(crate) oidc_issuer: String,
}

impl OpenID {
    pub fn auth_url(
        oidc_client_id: String,
        oidc_client_secret: String,
        oidc_issuer: String,
    ) -> (Url, CsrfToken, CoreClient, Nonce, PkceCodeVerifier) {
        let oidc_client_id = ClientId::new(oidc_client_id);
        let oidc_client_secret = ClientSecret::new(oidc_client_secret);
        let oidc_issuer = IssuerUrl::new(oidc_issuer).expect("Missing the OIDC_ISSUER.");

        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let provider_metadata = CoreProviderMetadata::discover(&oidc_issuer, http_client)
            .unwrap_or_else(|_err| {
                println!("Failed to discover OpenID Provider");
                unreachable!();
            });

        let client = CoreClient::from_provider_metadata(
            provider_metadata,
            oidc_client_id,
            Some(oidc_client_secret),
        )
        .set_redirect_uri(
            RedirectUrl::new("http://localhost:8080".to_string()).expect("Invalid redirect URL"),
        );

        let (authorize_url, csrf_state, nonce) = client
            .authorize_url(
                AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("email".to_string()))
            .add_scope(Scope::new("profile".to_string()))
            .set_pkce_challenge(pkce_challenge)
            .url();
        return (authorize_url, csrf_state, client, nonce, pkce_verifier);
    }
}

pub fn redirect_listener(
    _csrf_state: CsrfToken,
    client: CoreClient,
    nonce: Nonce,
    pkce_verifier: PkceCodeVerifier,
) -> Result<String> {
    println!("Requesting access token...\n");
    if let Some(stream) = TcpListener::bind("127.0.0.1:8000")?.incoming().next() {
        let mut stream = stream?;

        let mut reader = BufReader::new(&stream);

        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;

        let redirect_url = request_line.split_whitespace().nth(1).unwrap();
        let url = Url::parse(&("http://localhost".to_string() + redirect_url))?;

        let (_, value) = url
            .query_pairs()
            .find(|pair| {
                let &(ref key, _) = pair;
                key == "code"
            })
            .unwrap();

        let code = AuthorizationCode::new(value.into_owned());

        let (_, value) = url
            .query_pairs()
            .find(|pair| {
                let &(ref key, _) = pair;
                key == "state"
            })
            .unwrap();

        let state = CsrfToken::new(value.into_owned());

        let html_page = "<html><title>Sigstore Auth</title><body><h1>Sigstore Auth Successful</h1><p>You may now close this page.</p></body></html>";

        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
            html_page.len(),
            html_page
        );
        stream.write_all(response.as_bytes())?;

        // Exchange the code with a token.
        let token_response = client
            .exchange_code(code)
            .set_pkce_verifier(pkce_verifier)
            .request(http_client)
            .map_err(|err| {
                SigstoreError::UnexpectedError(format!("Failed to access token endpoint: {}", err))
            })?;

        let id_token_verifier: CoreIdTokenVerifier = client.id_token_verifier();
        let id_token_claims: &CoreIdTokenClaims = token_response
            .extra_fields()
            .id_token()
            .expect("Server did not return an ID token")
            .claims(&id_token_verifier, &nonce)
            .map_err(|err| {
                SigstoreError::UnexpectedError(format!("Failed to verify ID token: {}", err))
            })?;

        println!("ID TOKEN CLAIMS: {:?}", id_token_claims.access_token_hash());

        match id_token_claims.email() {
            Some(email) => println!("ID TOKEN CLAIMS: {:?}", email),
            None => println!("ID TOKEN CLAIMS: {:?}", "No email found"),
        }

        let email = id_token_claims.email().expect("No email found").to_string();
        println!("EMAIL: {:?}", email);

        println!(
            "User scope granted with e-mail address {}",
            id_token_claims
                .email()
                .map(|email| email.as_str())
                .unwrap_or("<not provided>"),
        );

        return Ok(email);
    }
    unreachable!()
}
