//! YouTube's streaming live chat (`streamList`) over gRPC, in the shape [`crate::chat`] reads.

use std::time::Duration;

use serde_json::{Value, json};
use tonic::Code;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use pb::live_chat_message_snippet::DisplayedContent;
use pb::live_chat_message_snippet::type_wrapper::Type;

#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod pb {
    tonic::include_proto!("youtube.api.v3");
}

/// YouTube's gRPC endpoint.
pub const ENDPOINT: &str = "https://youtube.googleapis.com";

/// Why a stream couldn't start or stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamError {
    /// The access token was refused; refresh it and try again.
    Unauthorized,
    Quota,
    ChatEnded,
    /// The endpoint doesn't serve this method (or this account); poll instead.
    Unsupported(String),
    Lost(String),
}

/// Opens a connection to the endpoint (TLS for `https://`).
pub async fn channel(endpoint: &str) -> Result<Channel, StreamError> {
    let mut ep = Endpoint::from_shared(endpoint.to_string())
        .map_err(|e| StreamError::Unsupported(format!("YouTube: bad stream address ({e}).")))?
        .connect_timeout(Duration::from_secs(15))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_while_idle(true);
    if endpoint.starts_with("https://") {
        ep = ep
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .map_err(|e| StreamError::Unsupported(format!("YouTube: TLS setup failed ({e}).")))?;
    }
    ep.connect().await.map_err(|e| {
        StreamError::Lost(format!(
            "YouTube: can't reach the chat stream ({e}). Check the internet connection."
        ))
    })
}

/// Starts streaming a chat, from `page_token` when resuming.
pub async fn open(
    channel: Channel,
    access: &str,
    chat: &str,
    page_token: Option<&str>,
) -> Result<tonic::Streaming<pb::LiveChatMessageListResponse>, StreamError> {
    let mut client =
        pb::v3_data_live_chat_message_service_client::V3DataLiveChatMessageServiceClient::new(
            channel,
        );
    let mut req = tonic::Request::new(pb::LiveChatMessageListRequest {
        live_chat_id: Some(chat.to_string()),
        part: vec!["id".into(), "snippet".into(), "authorDetails".into()],
        page_token: page_token.map(str::to_string),
        ..Default::default()
    });
    let bearer = format!("Bearer {access}")
        .parse()
        .map_err(|_| StreamError::Unauthorized)?;
    req.metadata_mut().insert("authorization", bearer);
    client
        .stream_list(req)
        .await
        .map(tonic::Response::into_inner)
        .map_err(status_error)
}

/// Maps a gRPC status to what the chat loop does about it.
pub fn status_error(s: tonic::Status) -> StreamError {
    let message = s.message().to_string();
    let lower = message.to_lowercase();
    match s.code() {
        Code::Unauthenticated => StreamError::Unauthorized,
        Code::ResourceExhausted => StreamError::Quota,
        Code::PermissionDenied if lower.contains("quota") => StreamError::Quota,
        Code::Unimplemented => StreamError::Unsupported(format!(
            "YouTube: the chat stream isn't available ({message})."
        )),
        Code::NotFound | Code::FailedPrecondition | Code::PermissionDenied
            if lower.contains("ended")
                || lower.contains("not found")
                || lower.contains("disabled") =>
        {
            StreamError::ChatEnded
        }
        code => StreamError::Lost(format!(
            "YouTube: the chat stream stopped ({code:?}: {message})."
        )),
    }
}

/// The REST name of a message type.
fn type_name(t: Type) -> &'static str {
    match t {
        Type::TextMessageEvent => "textMessageEvent",
        Type::Tombstone => "messageDeletedEvent",
        Type::FanFundingEvent => "fanFundingEvent",
        Type::ChatEndedEvent => "chatEndedEvent",
        Type::SponsorOnlyModeStartedEvent => "sponsorOnlyModeStartedEvent",
        Type::SponsorOnlyModeEndedEvent => "sponsorOnlyModeEndedEvent",
        Type::NewSponsorEvent => "newSponsorEvent",
        Type::MemberMilestoneChatEvent => "memberMilestoneChatEvent",
        Type::MembershipGiftingEvent => "membershipGiftingEvent",
        Type::GiftMembershipReceivedEvent => "giftMembershipReceivedEvent",
        Type::UserBannedEvent => "userBannedEvent",
        Type::SuperChatEvent => "superChatEvent",
        Type::SuperStickerEvent => "superStickerEvent",
        Type::PollEvent => "pollEvent",
        Type::GiftEvent => "giftEvent",
        Type::InvalidType => "invalidType",
    }
}

fn item(m: &pb::LiveChatMessage) -> Value {
    let id = m.id.clone().unwrap_or_default();
    let mut snippet = json!({});
    if let Some(s) = &m.snippet {
        let kind = s
            .r#type
            .and_then(|t| Type::try_from(t).ok())
            .unwrap_or(Type::InvalidType);
        snippet = json!({
            "type": type_name(kind),
            "authorChannelId": s.author_channel_id,
            "publishedAt": s.published_at,
            "hasDisplayContent": s.has_display_content,
            "displayMessage": s.display_message,
        });
        // A tombstone takes the place of a deleted message, under the same ID.
        if kind == Type::Tombstone {
            snippet["messageDeletedDetails"] = json!({"deletedMessageId": id});
        }
        match &s.displayed_content {
            Some(DisplayedContent::TextMessageDetails(d)) => {
                snippet["textMessageDetails"] = json!({"messageText": d.message_text});
            }
            Some(DisplayedContent::UserBannedDetails(d)) => {
                let user = d.banned_user_details.as_ref();
                snippet["userBannedDetails"] = json!({
                    "bannedUserDetails": {
                        "channelId": user.and_then(|u| u.channel_id.clone()),
                        "displayName": user.and_then(|u| u.display_name.clone()),
                    },
                    "banType": match d.ban_type {
                        Some(2) => "temporary",
                        _ => "permanent",
                    },
                    "banDurationSeconds": d.ban_duration_seconds,
                });
            }
            Some(DisplayedContent::SuperChatDetails(d)) => {
                snippet["superChatDetails"] = json!({
                    "amountDisplayString": d.amount_display_string,
                    "userComment": d.user_comment,
                });
            }
            Some(DisplayedContent::SuperStickerDetails(d)) => {
                snippet["superStickerDetails"] =
                    json!({"amountDisplayString": d.amount_display_string});
            }
            _ => {}
        }
    }
    let a = m.author_details.clone().unwrap_or_default();
    json!({
        "id": id,
        "snippet": snippet,
        "authorDetails": {
            "channelId": a.channel_id,
            "displayName": a.display_name,
            "profileImageUrl": a.profile_image_url,
            "isVerified": a.is_verified,
            "isChatOwner": a.is_chat_owner,
            "isChatSponsor": a.is_chat_sponsor,
            "isChatModerator": a.is_chat_moderator,
        },
    })
}

/// One streamed reply in the shape of a `liveChatMessages.list` response.
pub fn to_json(r: &pb::LiveChatMessageListResponse) -> Value {
    let ended = r
        .items
        .iter()
        .any(|m| m.snippet.as_ref().and_then(|s| s.r#type) == Some(Type::ChatEndedEvent as i32));
    let mut v = json!({
        "items": r.items.iter().map(item).collect::<Vec<_>>(),
        "nextPageToken": r.next_page_token,
    });
    if let Some(at) = &r.offline_at {
        v["offlineAt"] = json!(at);
    } else if ended {
        v["offlineAt"] = json!("");
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat;
    use rill_core::Event;

    pub(crate) fn text(id: &str, author: &str, text: &str) -> pb::LiveChatMessage {
        pb::LiveChatMessage {
            id: Some(id.into()),
            snippet: Some(pb::LiveChatMessageSnippet {
                r#type: Some(Type::TextMessageEvent as i32),
                author_channel_id: Some(format!("UC{author}")),
                published_at: Some("2026-11-06T12:00:00Z".into()),
                has_display_content: Some(true),
                display_message: Some(text.into()),
                displayed_content: Some(DisplayedContent::TextMessageDetails(
                    pb::LiveChatTextMessageDetails {
                        message_text: Some(text.into()),
                    },
                )),
                ..Default::default()
            }),
            author_details: Some(pb::LiveChatMessageAuthorDetails {
                channel_id: Some(format!("UC{author}")),
                display_name: Some(author.into()),
                is_chat_moderator: Some(author == "mod"),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn streamed_replies_read_like_list_responses() {
        let mut tomb = text("m1", "a", "");
        tomb.snippet.as_mut().unwrap().r#type = Some(Type::Tombstone as i32);
        let r = pb::LiveChatMessageListResponse {
            items: vec![text("m2", "mod", "hello"), tomb],
            next_page_token: Some("p2".into()),
            ..Default::default()
        };
        let page = chat::page(&to_json(&r));
        assert_eq!(page.next_page_token.as_deref(), Some("p2"));
        assert!(!page.offline);
        let Event::Message(m) = &page.events[0] else {
            panic!("{:?}", page.events)
        };
        assert_eq!(m.plain_text(), "hello");
        assert!(m.author.roles.moderator);
        assert_eq!(m.id, "m2");
        assert!(matches!(&page.events[1], Event::Deleted { message_id, .. } if message_id == "m1"));

        let mut end = text("m3", "a", "");
        end.snippet.as_mut().unwrap().r#type = Some(Type::ChatEndedEvent as i32);
        let r = pb::LiveChatMessageListResponse {
            items: vec![end],
            ..Default::default()
        };
        assert!(chat::page(&to_json(&r)).offline);
    }

    /// Against YouTube itself, with a token it must refuse; run with `--ignored`.
    #[tokio::test]
    #[ignore = "talks to youtube.googleapis.com"]
    async fn live_endpoint_answers_and_refuses_a_bad_token() {
        let channel = channel(ENDPOINT).await.expect("TLS connection");
        // A server stream reports errors with its first reply, not when it opens.
        let err = match open(channel, "not-a-token", "no-such-chat", None).await {
            Err(e) => e,
            Ok(mut s) => status_error(s.message().await.expect_err("a bad token must fail")),
        };
        assert_eq!(
            err,
            StreamError::Unauthorized,
            "the method exists and checks the token"
        );
    }

    #[test]
    fn statuses_decide_what_happens_next() {
        let s = |code, msg: &str| status_error(tonic::Status::new(code, msg));
        assert_eq!(s(Code::Unauthenticated, "x"), StreamError::Unauthorized);
        assert_eq!(s(Code::ResourceExhausted, "x"), StreamError::Quota);
        assert_eq!(
            s(Code::PermissionDenied, "quotaExceeded"),
            StreamError::Quota
        );
        assert_eq!(s(Code::NotFound, "liveChatEnded"), StreamError::ChatEnded);
        assert!(matches!(
            s(Code::Unimplemented, "x"),
            StreamError::Unsupported(_)
        ));
        assert!(matches!(s(Code::Unavailable, "x"), StreamError::Lost(_)));
    }
}
