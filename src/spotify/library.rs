//! Pure mapping and validation for Spotify library destinations.
use librespot::protocol::playlist4_external::MetaItem;

use super::{DJ_PLAYLIST_ID, PlaylistSummary};

pub fn playlist_summary(uri: &str, meta: Option<&MetaItem>) -> PlaylistSummary {
    let is_dj = uri == format!("spotify:playlist:{DJ_PLAYLIST_ID}");
    let attrs = meta.and_then(|m| m.attributes.as_ref());
    let name = if is_dj {
        "DJ".to_owned()
    } else {
        attrs.and_then(|a| a.name.as_deref()).filter(|s| !s.trim().is_empty()).unwrap_or("Playlist").to_owned()
    };
    let image_url = attrs.and_then(|a| {
        a.picture_size.iter()
            .filter_map(|p| p.url.as_deref().filter(|u| u.starts_with("https://")).map(|url| (p.target_name.as_deref() == Some("default"), url)))
            .max_by_key(|(default, _)| *default)
            .map(|(_, url)| url.to_owned())
            .or_else(|| a.picture.as_deref().filter(|p| p.len() == 20).map(|p| format!("https://i.scdn.co/image/{}", hex::encode(p))))
    });
    PlaylistSummary {
        uri: uri.to_owned(), name, image_url,
        can_add_tracks: !is_dj && meta.and_then(|m| m.capabilities.as_ref()).and_then(|c| c.can_edit_items) == Some(true),
        song_count: meta.and_then(|m| m.length).and_then(|n| u32::try_from(n).ok()),
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub fn track_uri_is_valid(uri: &str) -> bool {
    uri.strip_prefix("spotify:track:").is_some_and(valid_id)
}

pub fn playlist_id(uri: &str) -> Option<&str> {
    let id = uri.strip_prefix("spotify:playlist:")?;
    (valid_id(id) && id != DJ_PLAYLIST_ID).then_some(id)
}

pub fn collection_uri(username: &str) -> String {
    format!("spotify:user:{username}:collection")
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot::protocol::{playlist4_external::{ListAttributes, PictureSize}, playlist_permission::Capabilities};
    use protobuf::MessageField;

    #[test]
    fn metadata_has_unicode_name_and_explicit_permission() {
        let mut meta = MetaItem { length: Some(42), attributes: MessageField::some(ListAttributes {
            name: Some("lạc lối".into()), picture: Some(vec![0xab; 20]), ..Default::default()
        }), ..Default::default() };
        let uri = "spotify:playlist:5ddDghRcNt3q3Ytp18TvbD";
        assert!(!playlist_summary(uri, Some(&meta)).can_add_tracks);
        meta.capabilities = MessageField::some(Capabilities { can_edit_items: Some(true), ..Default::default() });
        let p = playlist_summary(uri, Some(&meta));
        assert_eq!(p.name, "lạc lối");
        assert!(p.can_add_tracks);
        assert_eq!(p.song_count, Some(42));
        assert_eq!(p.image_url, Some(format!("https://i.scdn.co/image/{}", "ab".repeat(20))));
        let dj = playlist_summary(&format!("spotify:playlist:{DJ_PLAYLIST_ID}"), Some(&meta));
        assert_eq!(dj.name, "DJ");
        assert!(!dj.can_add_tracks);
    }

    #[test]
    fn prefers_default_https_and_handles_missing_fields() {
        let meta = MetaItem { attributes: MessageField::some(ListAttributes {
            picture_size: vec![
                PictureSize { target_name: Some("default".into()), url: Some("http://bad".into()), ..Default::default() },
                PictureSize { target_name: Some("small".into()), url: Some("https://small".into()), ..Default::default() },
                PictureSize { target_name: Some("default".into()), url: Some("https://default".into()), ..Default::default() },
            ], ..Default::default()
        }), length: Some(-1), ..Default::default() };
        let p = playlist_summary("spotify:playlist:5ddDghRcNt3q3Ytp18TvbD", Some(&meta));
        assert_eq!(p.image_url.as_deref(), Some("https://default"));
        assert_eq!(p.song_count, None);
        let missing = playlist_summary("spotify:playlist:5ddDghRcNt3q3Ytp18TvbD", None);
        assert_eq!(missing.name, "Playlist");
        assert!(missing.image_url.is_none());
        assert!(!missing.can_add_tracks);
    }

    #[test]
    fn uris_are_exact_ascii_base62_and_dj_is_never_destination() {
        assert!(track_uri_is_valid("spotify:track:4uLU6hMCjMI75M1A2tKUQC"));
        for uri in ["spotify:track:", "spotify:album:4uLU6hMCjMI75M1A2tKUQC", "spotify:track:4uLU6hMCjMI75M1A2tKUQC?x", "spotify:track:ééééééééééé"] {
            assert!(!track_uri_is_valid(uri), "{uri}");
        }
        assert_eq!(playlist_id("spotify:playlist:5ddDghRcNt3q3Ytp18TvbD"), Some("5ddDghRcNt3q3Ytp18TvbD"));
        assert!(playlist_id(&format!("spotify:playlist:{DJ_PLAYLIST_ID}")).is_none());
        assert!(playlist_id("spotify:playlist:!!!!!!!!!!!!!!!!!!!!!!").is_none());
        assert_eq!(collection_uri("user"), "spotify:user:user:collection");
    }
}
