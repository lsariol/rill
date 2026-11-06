//! 7TV's REST API: a user's active emote set, the global set, and single emotes.

use std::collections::HashMap;

use serde::Deserialize;

use crate::{Emote, EmoteSet};

/// 7TV's API and event socket addresses; tests point them at local servers.
#[derive(Debug, Clone)]
pub struct Config {
    pub api: String,
    pub events: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            api: "https://7tv.io/v3".into(),
            events: "wss://events.7tv.io/v3".into(),
        }
    }
}

#[derive(Deserialize)]
struct UserJson {
    emote_set: Option<SetJson>,
    user: Option<UserRef>,
}

#[derive(Deserialize)]
struct UserRef {
    id: String,
}

#[derive(Deserialize)]
pub(crate) struct SetJson {
    pub id: String,
    #[serde(default)]
    pub emotes: Option<Vec<ActiveJson>>,
}

/// An emote as it sits in a set: the name chat uses may differ from the emote's own.
#[derive(Deserialize, Clone)]
pub(crate) struct ActiveJson {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub flags: u32,
    pub data: Option<DataJson>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct DataJson {
    #[serde(default)]
    pub flags: u32,
    pub host: HostJson,
}

#[derive(Deserialize, Clone)]
pub(crate) struct HostJson {
    pub url: String,
    pub files: Vec<FileJson>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct FileJson {
    pub name: String,
    pub width: u16,
    pub height: u16,
}

/// The set entry's "zero-width" flag.
const ACTIVE_ZERO_WIDTH: u32 = 1;
/// The emote's own "zero-width" flag.
const EMOTE_ZERO_WIDTH: u32 = 1 << 8;

impl ActiveJson {
    /// The emote, if its image data came along.
    pub fn emote(&self) -> Option<Emote> {
        let data = self.data.as_ref()?;
        Emote::from_host(
            &self.id,
            &self.name,
            &data.host,
            self.flags & ACTIVE_ZERO_WIDTH != 0 || data.flags & EMOTE_ZERO_WIDTH != 0,
        )
    }
}

impl Emote {
    /// Picks the 1x WebP (animated where the emote is), falling back to any 1x file.
    pub(crate) fn from_host(id: &str, name: &str, host: &HostJson, overlay: bool) -> Option<Emote> {
        let file = host
            .files
            .iter()
            .find(|f| f.name == "1x.webp")
            .or_else(|| host.files.iter().find(|f| f.name.starts_with("1x.")))?;
        let base = host.url.trim_end_matches('/');
        let base = if base.starts_with("//") {
            format!("https:{base}")
        } else {
            base.to_string()
        };
        Some(Emote {
            id: id.to_string(),
            name: name.to_string(),
            url: format!("{base}/{}", file.name),
            size: (file.width.max(1), file.height.max(1)),
            overlay,
        })
    }
}

/// A set's emotes by the name chat uses. Entries without image data are skipped.
pub(crate) fn to_set(set: &SetJson) -> EmoteSet {
    let mut out = HashMap::new();
    for e in set.emotes.iter().flatten() {
        if let Some(emote) = e.emote() {
            out.insert(emote.name.clone(), emote);
        }
    }
    out
}

/// A channel's 7TV account: its user ID, active set ID, and emotes.
pub(crate) struct Channel {
    pub user_id: Option<String>,
    pub set_id: Option<String>,
    pub emotes: EmoteSet,
}

async fn get(http: &reqwest::Client, url: &str) -> Result<Option<Vec<u8>>, String> {
    let res = http
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("7TV: can't reach 7tv.io ({e}). Check the internet connection."))?;
    match res.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        s => {
            return Err(format!(
                "7TV: the API answered {s}; Rill will try again later."
            ));
        }
    }
    let bytes = res
        .bytes()
        .await
        .map_err(|e| format!("7TV: the download broke off ({e})."))?;
    Ok(Some(bytes.to_vec()))
}

/// The first of `ids` that has a 7TV account on `platform` ("twitch" or "kick"), or `None`.
pub(crate) async fn channel(
    http: &reqwest::Client,
    cfg: &Config,
    platform: &str,
    ids: &[String],
) -> Result<Option<Channel>, String> {
    for id in ids {
        let Some(body) = get(http, &format!("{}/users/{platform}/{id}", cfg.api)).await? else {
            continue;
        };
        let user: UserJson = serde_json::from_slice(&body)
            .map_err(|e| format!("7TV: unexpected user data ({e}); 7TV may have changed it."))?;
        return Ok(Some(Channel {
            user_id: user.user.map(|u| u.id),
            set_id: user.emote_set.as_ref().map(|s| s.id.clone()),
            emotes: user.emote_set.as_ref().map(to_set).unwrap_or_default(),
        }));
    }
    Ok(None)
}

/// 7TV's global emotes.
pub(crate) async fn global(http: &reqwest::Client, cfg: &Config) -> Result<EmoteSet, String> {
    let body = get(http, &format!("{}/emote-sets/global", cfg.api))
        .await?
        .ok_or("7TV: the global emote set is missing (404).")?;
    let set: SetJson = serde_json::from_slice(&body)
        .map_err(|e| format!("7TV: unexpected emote set data ({e}); 7TV may have changed it."))?;
    Ok(to_set(&set))
}

#[derive(Deserialize)]
struct EmoteJson {
    #[serde(default)]
    flags: u32,
    host: HostJson,
}

/// One emote's image data, for set changes that arrive without it.
pub(crate) async fn emote(
    http: &reqwest::Client,
    cfg: &Config,
    active: &ActiveJson,
) -> Result<Option<Emote>, String> {
    let Some(body) = get(http, &format!("{}/emotes/{}", cfg.api, active.id)).await? else {
        return Ok(None);
    };
    let e: EmoteJson =
        serde_json::from_slice(&body).map_err(|e| format!("7TV: unexpected emote data ({e})."))?;
    Ok(Emote::from_host(
        &active.id,
        &active.name,
        &e.host,
        active.flags & ACTIVE_ZERO_WIDTH != 0 || e.flags & EMOTE_ZERO_WIDTH != 0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = include_str!("../tests/fixtures/user.json");

    #[test]
    fn reads_a_users_set_with_aliases_sizes_and_zero_width() {
        let user: UserJson = serde_json::from_str(USER).unwrap();
        assert_eq!(user.user.unwrap().id, "U1");
        let set = to_set(user.emote_set.as_ref().unwrap());
        assert_eq!(set.len(), 3, "the entry without image data is skipped");
        let wide = &set["wideDance"];
        assert_eq!(wide.url, "https://cdn.7tv.app/emote/E1/1x.webp");
        assert_eq!(wide.size, (96, 32));
        assert!(!wide.overlay);
        // Chat uses the set's name for an emote, not the emote's own.
        assert!(set.contains_key("myAlias") && !set.contains_key("OriginalName"));
        assert!(set["rainOver"].overlay, "the emote's zero-width flag");
        assert!(set["myAlias"].overlay, "the set entry's zero-width flag");
    }
}
