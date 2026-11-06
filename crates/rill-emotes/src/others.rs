//! BetterTTV and FrankerFaceZ: global sets and Twitch channels' sets. Neither changes live here;
//! they load when a chat connects.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::{Emote, EmoteSet};

#[derive(Deserialize)]
struct BttvEmote {
    id: String,
    code: String,
    /// Modifiers (such as `cvMask`) are drawn over the emote before them.
    #[serde(default)]
    modifier: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BttvUser {
    #[serde(default)]
    channel_emotes: Vec<BttvEmote>,
    #[serde(default)]
    shared_emotes: Vec<BttvEmote>,
}

fn bttv_set(emotes: impl IntoIterator<Item = BttvEmote>) -> EmoteSet {
    emotes
        .into_iter()
        .map(|e| {
            let emote = Emote {
                url: format!("https://cdn.betterttv.net/emote/{}/1x.webp", e.id),
                id: e.id,
                name: e.code.clone(),
                // BTTV doesn't say; the row keeps the image's own proportions at emote height.
                size: (0, 0),
                overlay: e.modifier,
            };
            (e.code, emote)
        })
        .collect()
}

async fn get(http: &reqwest::Client, url: &str, what: &str) -> Result<Option<Vec<u8>>, String> {
    let res = http
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("{what}: can't reach the emote service ({e})."))?;
    match res.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        s => return Err(format!("{what}: the emote service answered {s}.")),
    }
    let body = res
        .bytes()
        .await
        .map_err(|e| format!("{what}: the download broke off ({e})."))?;
    Ok(Some(body.to_vec()))
}

pub(crate) async fn bttv_global(http: &reqwest::Client, base: &str) -> Result<EmoteSet, String> {
    let Some(body) = get(http, &format!("{base}/cached/emotes/global"), "BetterTTV").await? else {
        return Ok(EmoteSet::new());
    };
    let emotes: Vec<BttvEmote> = serde_json::from_slice(&body)
        .map_err(|e| format!("BetterTTV: unexpected emote data ({e})."))?;
    Ok(bttv_set(emotes))
}

pub(crate) async fn bttv_channel(
    http: &reqwest::Client,
    base: &str,
    twitch_id: &str,
) -> Result<EmoteSet, String> {
    let url = format!("{base}/cached/users/twitch/{twitch_id}");
    let Some(body) = get(http, &url, "BetterTTV").await? else {
        return Ok(EmoteSet::new());
    };
    let user: BttvUser = serde_json::from_slice(&body)
        .map_err(|e| format!("BetterTTV: unexpected channel data ({e})."))?;
    Ok(bttv_set(
        user.shared_emotes.into_iter().chain(user.channel_emotes),
    ))
}

/// FFZ sets by ID; each holds `emoticons`.
fn ffz_emotes(sets: &Value, only: Option<&[u64]>) -> EmoteSet {
    let mut out = HashMap::new();
    let Some(sets) = sets.as_object() else {
        return out;
    };
    for (id, set) in sets {
        if let Some(only) = only
            && !id.parse::<u64>().is_ok_and(|id| only.contains(&id))
        {
            continue;
        }
        for e in set["emoticons"].as_array().into_iter().flatten() {
            let (Some(name), Some(url)) = (e["name"].as_str(), e["urls"]["1"].as_str()) else {
                continue;
            };
            let url = if url.starts_with("//") {
                format!("https:{url}")
            } else {
                url.to_string()
            };
            let dim = |k: &str| {
                e[k].as_u64()
                    .and_then(|v| u16::try_from(v).ok())
                    .unwrap_or(0)
            };
            out.insert(
                name.to_string(),
                Emote {
                    id: e["id"].to_string(),
                    name: name.to_string(),
                    url,
                    size: (dim("width"), dim("height")),
                    overlay: e["modifier"].as_bool().unwrap_or(false),
                },
            );
        }
    }
    out
}

pub(crate) async fn ffz_global(http: &reqwest::Client, base: &str) -> Result<EmoteSet, String> {
    let Some(body) = get(http, &format!("{base}/set/global"), "FrankerFaceZ").await? else {
        return Ok(EmoteSet::new());
    };
    let v: Value = serde_json::from_slice(&body)
        .map_err(|e| format!("FrankerFaceZ: unexpected emote data ({e})."))?;
    let defaults: Vec<u64> = v["default_sets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect();
    Ok(ffz_emotes(&v["sets"], Some(&defaults)))
}

pub(crate) async fn ffz_channel(
    http: &reqwest::Client,
    base: &str,
    twitch_id: &str,
) -> Result<EmoteSet, String> {
    let url = format!("{base}/room/id/{twitch_id}");
    let Some(body) = get(http, &url, "FrankerFaceZ").await? else {
        return Ok(EmoteSet::new());
    };
    let v: Value = serde_json::from_slice(&body)
        .map_err(|e| format!("FrankerFaceZ: unexpected channel data ({e})."))?;
    Ok(ffz_emotes(&v["sets"], None))
}

/// A Twitch channel's custom moderator and VIP badges on FrankerFaceZ, as image addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomBadges {
    pub moderator: Option<String>,
    pub vip: Option<String>,
}

/// Reads the custom badges from a FrankerFaceZ room (the biggest image of each).
pub(crate) fn room_badges(room: &Value) -> RoomBadges {
    let biggest = |v: &Value| {
        ["4", "2", "1"]
            .iter()
            .find_map(|k| v[*k].as_str())
            .map(str::to_string)
    };
    RoomBadges {
        moderator: biggest(&room["mod_urls"])
            .or_else(|| room["moderator_badge"].as_str().map(str::to_string)),
        vip: biggest(&room["vip_badge"]),
    }
}

pub(crate) async fn ffz_room_badges(
    http: &reqwest::Client,
    base: &str,
    twitch_id: &str,
) -> Result<RoomBadges, String> {
    let url = format!("{base}/room/id/{twitch_id}");
    let Some(body) = get(http, &url, "FrankerFaceZ").await? else {
        return Ok(RoomBadges::default());
    };
    let v: Value = serde_json::from_slice(&body)
        .map_err(|e| format!("FrankerFaceZ: unexpected channel data ({e})."))?;
    Ok(room_badges(&v["room"]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_custom_room_badges() {
        let room = json!({
            "moderator_badge": "https://ffz/mod/1",
            "mod_urls": {"1": "https://ffz/mod/1", "2": "https://ffz/mod/2"},
            "vip_badge": {"1": "https://ffz/vip/1", "4": "https://ffz/vip/4"}
        });
        assert_eq!(
            room_badges(&room),
            RoomBadges {
                moderator: Some("https://ffz/mod/2".into()),
                vip: Some("https://ffz/vip/4".into()),
            }
        );
        let plain = json!({"moderator_badge": null, "vip_badge": null, "mod_urls": null});
        assert_eq!(room_badges(&plain), RoomBadges::default());
    }

    #[test]
    fn reads_bttv_and_ffz_sets() {
        let user: BttvUser = serde_json::from_value(json!({
            "channelEmotes": [{"id": "b1", "code": "catJAM", "imageType": "gif", "animated": true}],
            "sharedEmotes": [{"id": "b2", "code": "cvMask", "modifier": true}]
        }))
        .unwrap();
        let set = bttv_set(user.shared_emotes.into_iter().chain(user.channel_emotes));
        assert_eq!(
            set["catJAM"].url,
            "https://cdn.betterttv.net/emote/b1/1x.webp"
        );
        assert!(set["cvMask"].overlay);
        assert!(!set["catJAM"].overlay);

        let sets = json!({
            "3": {"emoticons": [{"id": 9, "name": "ZrehplaR", "width": 33, "height": 30,
                                 "urls": {"1": "//cdn.frankerfacez.com/emote/9/1"}}]},
            "7": {"emoticons": [{"id": 10, "name": "Other", "urls": {"1": "https://x/1"}}]}
        });
        let only = ffz_emotes(&sets, Some(&[3]));
        assert_eq!(only.len(), 1);
        let z = &only["ZrehplaR"];
        assert_eq!(
            (z.url.as_str(), z.size),
            ("https://cdn.frankerfacez.com/emote/9/1", (33, 30))
        );
        assert_eq!(ffz_emotes(&sets, None).len(), 2);
    }
}
