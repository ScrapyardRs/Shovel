use std::sync::Arc;

use drax::prelude::Uuid;
use drax::transport::encryption::{DecryptRead, Decryption, EncryptedWriter, Encryption};
use drax::{err_explain, throw_explain};
use hyper::body::to_bytes;
use hyper::{Body, Client};
use hyper_rustls::HttpsConnectorBuilder;
use mcprotocol::clientbound::login::ClientboundLoginRegistry::LoginDisconnect;
use mcprotocol::common::GameProfile;
use mcprotocol::serverbound::login::ServerBoundLoginRegsitry;
use num_bigint::BigInt;
use rand::RngCore;
use rsa::signature::digest::crypto_common::KeyIvInit;
use rsa::PaddingScheme;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::client::{MCClient, McPacketReader, McPacketWriter};
use crate::crypto::{private_key_to_der, MCPrivateKey};
use crate::phase::ConnectionInformation;

async fn call_mojang_auth(
    server: String,
    route: String,
    params: String,
) -> drax::prelude::Result<GameProfile> {
    let https = HttpsConnectorBuilder::new()
        .with_native_roots()
        .https_only()
        .enable_http1()
        .build();

    let client = Client::builder().build::<_, Body>(https);

    let url = format!("{server}{route}{params}");
    let mut url = url
        .parse::<hyper::Uri>()
        .map_err(|err| err_explain!(format!("Error parsing hyper URI: {}", err)))?;

    loop {
        let res = client
            .get(url)
            .await
            .map_err(|err| err_explain!(format!("Error sending request: {}", err)))?;

        match res.status().as_u16() {
            301 | 307 | 308 => {
                if let Some(redirect) = res.headers().get("Location") {
                    let redirect = redirect.to_str().map_err(|err| {
                        err_explain!(format!("Error converting redirect to string: {}", err))
                    })?;
                    url = redirect
                        .to_string()
                        .parse::<hyper::Uri>()
                        .map_err(|err| err_explain!(format!("Error parsing hyper URI: {}", err)))?;
                    continue;
                } else {
                    throw_explain!("No redirect location found!");
                }
            }
            x if x != 200 => {
                throw_explain!(format!("Mojang failed to auth, {}", res.status()))
            }
            200 => {}
            _ => {}
        }
        if res.status().as_u16() == 204 {
            throw_explain!("Mojang failed to auth; No profile found")
        } else if res.status().as_u16() != 200 {
            throw_explain!(format!("Mojang failed to auth, {}", res.status()))
        }

        let body = to_bytes(res.into_body()).await.map_err(|err| {
            err_explain!(format!("Failed to process bytes from response, {}", err))
        })?;

        let profile: GameProfile = match serde_json::from_slice(&body) {
            Ok(profile) => profile,
            Err(err) => {
                throw_explain!(format!("Error retrieving profile: {}", err))
            }
        };

        return Ok(profile);
    }
}

#[inline]
fn hash_server_id(server_id: &str, shared_secret: &[u8], public_key: &[u8]) -> String {
    use md5::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(server_id);
    hasher.update(shared_secret);
    hasher.update(public_key);
    let bytes = hasher.finalize();
    let bigint = BigInt::from_signed_bytes_be(bytes.as_slice());
    format!("{bigint:x}")
}

#[inline]
fn def_auth_server() -> String {
    "https://sessionserver.mojang.com".to_string()
}

enum LoginState {
    ExpectingHello,
    ExpectingKeyResponse {
        challenge: [u8; 4],
        name: String,
        profile_id: Option<Uuid>,
    },
}

pub async fn login_client<R, W>(
    key: Arc<MCPrivateKey>,
    mut read: R,
    mut write: W,
    connection_info: ConnectionInformation,
) -> drax::prelude::Result<MCClient<R, W>>
where
    R: AsyncRead + Unpin + Send + Sync,
    W: AsyncWrite + Unpin + Send + Sync,
{
    if connection_info.protocol_version != crate::version_constants::CURRENT_PROTOCOL_VERSION {
        write
            .write_packet(&LoginDisconnect {
                reason: format!(
                    "This server is running a different Minecraft version.\n\
                Please use {} to play on this server.",
                    crate::version_constants::CURRENT_PROTOCOL_VERSION_STRING
                )
                .into(),
            })
            .await?;
        throw_explain!("Outdated client attempted login.")
    }

    // loop until encryption response
    let mut state = LoginState::ExpectingHello;
    loop {
        match read.read_packet::<ServerBoundLoginRegsitry>().await? {
            ServerBoundLoginRegsitry::Hello { name, profile_id } => {
                if let LoginState::ExpectingHello = state {
                    let key_der = private_key_to_der(&key);
                    let mut verify_token = [0, 0, 0, 0];
                    rand::thread_rng().fill_bytes(&mut verify_token);
                    write
                        .write_packet(
                            &mcprotocol::clientbound::login::ClientboundLoginRegistry::Hello {
                                server_id: "".to_string(),
                                public_key: key_der,
                                challenge: verify_token.to_vec(),
                            },
                        )
                        .await?;
                    state = LoginState::ExpectingKeyResponse {
                        challenge: verify_token,
                        name,
                        profile_id,
                    };
                } else {
                    write
                        .write_packet(&LoginDisconnect {
                            reason: "Unexpected hello packet".into(),
                        })
                        .await?;
                    throw_explain!("Received unexpected hello packet")
                }
            }
            ServerBoundLoginRegsitry::Key {
                key_bytes,
                encrypted_challenge,
            } => {
                if let LoginState::ExpectingKeyResponse {
                    challenge,
                    name,
                    profile_id,
                } = state
                {
                    let decrypted_challenge = key
                        .decrypt(PaddingScheme::PKCS1v15Encrypt, &encrypted_challenge)
                        .map_err(|_| err_explain!("Failed to decrypt returned challenge."))?;
                    if decrypted_challenge.ne(&challenge) {
                        write
                            .write_packet(&LoginDisconnect {
                                reason: "Invalid challenge response.".into(),
                            })
                            .await?;
                        throw_explain!("Invalid challenge response.")
                    }

                    let shared_secret = key
                        .decrypt(PaddingScheme::PKCS1v15Encrypt, &key_bytes)
                        .map_err(|_| err_explain!("Failed to decrypt shared secret."))?;

                    let profile = call_mojang_auth(
                        def_auth_server(),
                        "/session/minecraft/hasJoined".to_string(),
                        format!(
                            "?username={}&serverId={}",
                            &name,
                            hash_server_id("", &shared_secret, &private_key_to_der(&key))
                        ),
                    )
                    .await?;

                    if matches!(&profile_id, Some(id) if id.ne(&profile.id)) {
                        write
                            .write_packet(&LoginDisconnect {
                                reason: "Invalid profile id".into(),
                            })
                            .await?;
                        throw_explain!("Invalid profile id")
                    }

                    let encryption = Encryption::new_from_slices(&shared_secret, &shared_secret);
                    let decryption = Decryption::new_from_slices(&shared_secret, &shared_secret);

                    match (encryption, decryption) {
                        (Ok(encryption), Ok(decryption)) => {
                            let client = MCClient::new(
                                DecryptRead::new(read, decryption),
                                EncryptedWriter::new(write, encryption),
                                connection_info,
                                name,
                                profile,
                            );

                            return Ok(client);
                        }
                        (Err(err), Ok(_)) | (Ok(_), Err(err)) => {
                            write
                                .write_packet(&LoginDisconnect {
                                    reason: "Failed to initialize encryption or decryption.".into(),
                                })
                                .await?;
                            throw_explain!(format!(
                                "Failed to initialize encryption or decryption: {err}"
                            ))
                        }
                        (Err(enc_err), Err(dec_err)) => {
                            write
                                .write_packet(&LoginDisconnect {
                                    reason: "Failed to initialize encryption and decryption."
                                        .into(),
                                })
                                .await?;
                            throw_explain!(format!(
                                "Failed to initialize encryption and decryption: {enc_err} + {dec_err}"
                            ))
                        }
                    }
                } else {
                    write
                        .write_packet(&LoginDisconnect {
                            reason: "Unexpected key packet.".into(),
                        })
                        .await?;
                    throw_explain!("Received key response before hello")
                }
            }
            ServerBoundLoginRegsitry::CustomQuery { .. } => {
                write
                    .write_packet(&LoginDisconnect {
                        reason: "Custom queries are not supported.".into(),
                    })
                    .await?;
                throw_explain!("Received custom query, unexpected during this state")
            }
        }
    }
}
