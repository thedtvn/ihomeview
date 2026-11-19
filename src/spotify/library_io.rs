//! Conservative, confirmation-based library reads and add-only writes.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use librespot::core::Session;
use librespot::protocol::playlist4_external::{self as p, SelectedListContent};
use protobuf::{CodedInputStream, CodedOutputStream, Message, MessageField, EnumOrUnknown};
use super::SaveTrackState;
use super::library::{playlist_id, track_uri_is_valid as valid_track, collection_uri};
#[cfg(test)]
use super::DJ_PLAYLIST_ID;

const PAGE: usize = 100;
const MAX_PAGES: usize = 100;
const COLLECTION_TYPE: &str = "application/vnd.collection-v2.spotify.proto";
const PLAYLIST_TYPE: &str = "application/x-protobuf";

fn now_ms() -> Result<i64, String> {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "Invalid system clock")?.as_millis();
    i64::try_from(ms).map_err(|_| "Invalid system clock".into())
}
fn collection_body(user: &str, track: &str, add: bool) -> Result<Vec<u8>, String> {
    let mut item = Vec::new();
    {
        let mut out = CodedOutputStream::vec(&mut item);
        out.write_string(1, track).map_err(|_| "Could not encode request")?;
        if add { out.write_int64(2, now_ms()? / 1000).map_err(|_| "Could not encode request")?; }
        out.flush().map_err(|_| "Could not encode request")?;
    }
    let mut body = Vec::new();
    {
        let mut out = CodedOutputStream::vec(&mut body);
        out.write_string(1, user).map_err(|_| "Could not encode request")?;
        out.write_string(2, "collection").map_err(|_| "Could not encode request")?;
        out.write_bytes(3, &item).map_err(|_| "Could not encode request")?;
        out.flush().map_err(|_| "Could not encode request")?;
    }
    Ok(body)
}
fn contains_response(bytes: &[u8]) -> Result<bool, String> {
    let mut input = CodedInputStream::from_bytes(bytes);
    let mut values = Vec::new();
    while !input.eof().map_err(|_| "Invalid collection response")? {
        let tag = input.read_raw_varint32().map_err(|_| "Invalid collection response")?;
        match tag {
            8 => values.push(input.read_bool().map_err(|_| "Invalid collection response")?),
            10 => {
                let packed = input.read_bytes().map_err(|_| "Invalid collection response")?;
                let mut nested = CodedInputStream::from_bytes(&packed);
                while !nested.eof().map_err(|_| "Invalid collection response")? {
                    values.push(nested.read_bool().map_err(|_| "Invalid collection response")?);
                }
            }
            _ => return Err("Invalid collection response".into()),
        }
    }
    if values.len() != 1 { return Err("Invalid collection response".into()); }
    Ok(values[0])
}
fn playlist_body(revision: &[u8], user: &str, track: &str) -> Result<Vec<u8>, String> {
    if revision.is_empty() { return Err("Playlist has no revision".into()); }
    let item = p::Item { uri: Some(track.to_owned()), ..Default::default() };
    let add = p::Add { items: vec![item], add_last: Some(true), ..Default::default() };
    let op = p::Op { kind: Some(EnumOrUnknown::new(p::op::Kind::ADD)), add: MessageField::some(add), ..Default::default() };
    let info = p::ChangeInfo { user: Some(user.to_owned()), timestamp: Some(now_ms()?), ..Default::default() };
    let delta = p::Delta { ops: vec![op], info: MessageField::some(info), ..Default::default() };
    p::ListChanges { base_revision: Some(revision.to_vec()), deltas: vec![delta], want_resulting_revisions: Some(true), ..Default::default() }
        .write_to_bytes().map_err(|_| "Could not encode playlist change".into())
}

struct Api { client: reqwest::Client, base: String, token: String, client_token: String }
impl Api {
    async fn new(session: &Session) -> Result<Self, String> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).redirect(reqwest::redirect::Policy::none()).retry(reqwest::retry::never()).build().map_err(|_| "Could not create Spotify client")?;
        let base = session.spclient().base_url().await.map_err(|_| "Could not resolve Spotify")?;
        let token = session.login5().auth_token().await.map_err(|_| "Could not authenticate with Spotify")?.access_token;
        let client_token = session.spclient().client_token().await.map_err(|_| "Could not authenticate Spotify client")?;
        Ok(Self { client, base, token, client_token })
    }
    async fn request(&self, method: reqwest::Method, path: &str, body: Option<(Vec<u8>, &str)>) -> Result<Vec<u8>, String> {
        let mut request = self.client.request(method, format!("{}{}", self.base.trim_end_matches('/'), path))
            .bearer_auth(&self.token).header("client-token", &self.client_token);
        if let Some((bytes, media)) = body { request = request.header("Content-Type", media).header("Accept", media).body(bytes); }
        let response = request.send().await.map_err(|_| "Spotify request failed".to_owned())?;
        let status = response.status();
        if !status.is_success() { return Err(if status.as_u16() == 409 { "Spotify reported a revision conflict".into() } else { format!("Spotify returned HTTP {}", status.as_u16()) }); }
        Ok(response.bytes().await.map_err(|_| "Could not read Spotify response")?.to_vec())
    }
    async fn contains(&self, user: &str, track: &str) -> Result<bool, String> {
        let bytes = self.request(reqwest::Method::POST, "/collection/v2/contains", Some((collection_body(user, track, false)?, COLLECTION_TYPE))).await?;
        contains_response(&bytes)
    }
    async fn rootlist(&self, session: &Session) -> Result<Vec<String>, String> {
        let mut result = Vec::new();
        let mut from = 0;
        let mut revision: Option<Vec<u8>> = None;
        for _ in 0..MAX_PAGES {
            let bytes = tokio::time::timeout(Duration::from_secs(20), session.spclient().get_rootlist(from, Some(PAGE))).await.map_err(|_| "Rootlist timed out")?.map_err(|_| "Could not read rootlist")?;
            let page = SelectedListContent::parse_from_bytes(&bytes).map_err(|_| "Invalid rootlist")?;
            check_revision(&mut revision, page.revision.as_deref())?;
            let length = page.length.and_then(|v| usize::try_from(v).ok()).ok_or("Rootlist length missing")?;
            let contents = page.contents.as_ref().ok_or("Rootlist contents missing")?;
            if contents.pos != Some(from as i32) { return Err("Unexpected rootlist page".into()); }
            for (index, item) in contents.items.iter().enumerate() {
                let uri = item.uri.as_deref().unwrap_or("");
                if playlist_id(uri).is_some() && contents.meta_items.get(index).and_then(|m| m.capabilities.as_ref()).and_then(|c| c.can_edit_items) == Some(true) && !result.iter().any(|s| s == uri) { result.push(uri.to_owned()); }
                else if playlist_id(uri).is_some() && contents.meta_items.get(index).and_then(|m| m.capabilities.as_ref()).and_then(|c| c.can_edit_items).is_none() { return Err("Playlist permissions unavailable".into()); }
            }
            from += contents.items.len();
            if from > length { return Err("Invalid rootlist length".into()); }
            if from == length && contents.truncated == Some(false) { return Ok(result); }
            if contents.items.is_empty() { return Err("Incomplete rootlist".into()); }
        }
        Err("Rootlist pagination limit reached".into())
    }
    async fn playlist(&self, id: &str, track: &str) -> Result<(bool, Vec<u8>), String> {
        let mut from = 0;
        let mut revision = None;
        let mut found = false;
        for _ in 0..MAX_PAGES {
            let path = format!("/playlist/v2/playlist/{id}?from={from}&length={PAGE}");
            let bytes = self.request(reqwest::Method::GET, &path, None).await?;
            let page = SelectedListContent::parse_from_bytes(&bytes).map_err(|_| "Invalid playlist response")?;
            check_revision(&mut revision, page.revision.as_deref())?;
            if page.capabilities.as_ref().and_then(|c| c.can_edit_items) != Some(true) { return Err("Playlist cannot be edited".into()); }
            let length = page.length.and_then(|v| usize::try_from(v).ok()).ok_or("Playlist length missing")?;
            let contents = page.contents.as_ref().ok_or("Playlist contents missing")?;
            if contents.pos != Some(from as i32) { return Err("Unexpected playlist page".into()); }
            found |= contents.items.iter().any(|item| item.uri.as_deref() == Some(track));
            from += contents.items.len();
            if from > length { return Err("Invalid playlist length".into()); }
            if from == length && contents.truncated == Some(false) { return Ok((found, revision.unwrap_or_default())); }
            if contents.items.is_empty() { return Err("Incomplete playlist".into()); }
        }
        Err("Playlist pagination limit reached".into())
    }
}
fn check_revision(current: &mut Option<Vec<u8>>, next: Option<&[u8]>) -> Result<(), String> {
    let next = next.filter(|r| !r.is_empty()).ok_or("Missing list revision")?;
    if current.as_deref().is_some_and(|r| r != next) { return Err("List changed while reading".into()); }
    *current = Some(next.to_vec());
    Ok(())
}

pub async fn inspect_saved(session: &Session, track_uri: &str) -> SaveTrackState {
    let mut state = SaveTrackState { track_uri: track_uri.to_owned(), ..Default::default() };
    if !valid_track(track_uri) { state.error = Some("Invalid track URI".into()); return state; }
    let api = match Api::new(session).await { Ok(v) => v, Err(e) => { state.error = Some(e); return state; } };
    let user = session.username();
    let mut errors = Vec::new();
    match api.contains(&user, track_uri).await { Ok(true) => state.saved_in.push(collection_uri(&user)), Ok(false) => (), Err(e) => errors.push(format!("Liked Songs: {e}")) }
    match api.rootlist(session).await {
        Ok(playlists) => for uri in playlists { match api.playlist(playlist_id(&uri).unwrap(), track_uri).await { Ok((true, _)) => state.saved_in.push(uri), Ok((false, _)) => (), Err(e) => errors.push(format!("{uri}: {e}")) } },
        Err(e) => errors.push(e),
    }
    if !errors.is_empty() { state.error = Some(format!("Could not inspect all destinations: {}", errors.join("; "))); }
    state
}

pub async fn save_track(session: &Session, track_uri: &str, destinations: &[String]) -> SaveTrackState {
    let mut state = SaveTrackState { track_uri: track_uri.to_owned(), ..Default::default() };
    if !valid_track(track_uri) || destinations.is_empty() { state.error = Some("Choose a valid track and destination".into()); return state; }
    let api = match Api::new(session).await { Ok(v) => v, Err(e) => { state.error = Some(e); return state; } };
    let user = session.username();
    let collection = collection_uri(&user);
    let allowed = match api.rootlist(session).await { Ok(v) => v, Err(e) => { state.error = Some(e); return state; } };
    if destinations.iter().any(|uri| uri != &collection && (playlist_id(uri).is_none() || !allowed.contains(uri))) { state.error = Some("A destination is not an editable playlist or your Liked Songs".into()); return state; }
    let mut errors = Vec::new();
    let mut seen = Vec::new();
    for uri in destinations {
        if seen.contains(uri) { continue; } seen.push(uri.clone());
        let is_collection = uri == &collection;
        let before = if is_collection { api.contains(&user, track_uri).await.map(|b| (b, Vec::new())) } else { api.playlist(playlist_id(uri).unwrap(), track_uri).await };
        let (already, revision) = match before { Ok(v) => v, Err(e) => { errors.push(format!("{uri}: {e}")); continue; } };
        if already { state.saved_in.push(uri.clone()); continue; }
        let body = if is_collection { collection_body(&user, track_uri, true) } else { playlist_body(&revision, &user, track_uri) };
        let body = match body { Ok(v) => v, Err(e) => { errors.push(format!("{uri}: {e}")); continue; } };
        let path = if is_collection { "/collection/v2/write".to_owned() } else { format!("/playlist/v2/playlist/{}/changes", playlist_id(uri).unwrap()) };
        let write = api.request(reqwest::Method::POST, &path, Some((body, if is_collection { COLLECTION_TYPE } else { PLAYLIST_TYPE }))).await;
        let confirmed = if is_collection { api.contains(&user, track_uri).await } else { api.playlist(playlist_id(uri).unwrap(), track_uri).await.map(|v| v.0) };
        if matches!(confirmed, Ok(true)) {
            state.saved_in.push(uri.clone());
            if let Err(e) = write { errors.push(format!("{uri}: {e}; membership is confirmed.")); }
        } else { errors.push(format!("{uri}: {}Could not confirm the save; check Spotify before retrying.", write.err().map(|e| format!("{e}. ")).unwrap_or_default())); }
    }
    if errors.is_empty() { state.notice = Some("Saved to the selected destinations.".into()); } else { state.error = Some(errors.join("; ")); }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn validates_uris_and_revision() {
        assert!(valid_track("spotify:track:1234567890123456789012"));
        assert!(!valid_track("spotify:track:bad"));
        assert!(playlist_id(&format!("spotify:playlist:{DJ_PLAYLIST_ID}")).is_none());
        assert!(playlist_body(&[], "user", "spotify:track:1234567890123456789012").is_err());
        let mut rev = None; assert!(check_revision(&mut rev, Some(b"a")).is_ok()); assert!(check_revision(&mut rev, Some(b"b")).is_err());
    }
    #[test] fn bodies_and_contains() {
        let b = collection_body("u", "spotify:track:1234567890123456789012", true).unwrap();
        let mut input = CodedInputStream::from_bytes(&b);
        assert_eq!(input.read_raw_varint32().unwrap(), 10); assert_eq!(input.read_string().unwrap(), "u");
        assert_eq!(input.read_raw_varint32().unwrap(), 18); assert_eq!(input.read_string().unwrap(), "collection");
        assert_eq!(input.read_raw_varint32().unwrap(), 26);
        let item = input.read_bytes().unwrap(); assert!(input.eof().unwrap());
        let mut nested = CodedInputStream::from_bytes(&item);
        assert_eq!(nested.read_raw_varint32().unwrap(), 10);
        assert_eq!(nested.read_string().unwrap(), "spotify:track:1234567890123456789012");
        assert_eq!(nested.read_raw_varint32().unwrap(), 16); assert!(nested.read_int64().unwrap() > 0); assert!(nested.eof().unwrap());
        assert_eq!(contains_response(&[8, 1]).unwrap(), true); assert_eq!(contains_response(&[10, 1, 0]).unwrap(), false);
        assert!(contains_response(&[]).is_err()); assert!(contains_response(&[8, 1, 8, 0]).is_err());
        let b = playlist_body(b"rev", "u", "spotify:track:1234567890123456789012").unwrap();
        let parsed = p::ListChanges::parse_from_bytes(&b).unwrap(); assert_eq!(parsed.base_revision.as_deref(), Some(&b"rev"[..])); assert_eq!(parsed.want_resulting_revisions, Some(true)); assert_eq!(parsed.deltas.len(), 1);
        let delta = &parsed.deltas[0]; assert_eq!(delta.info.as_ref().unwrap().user.as_deref(), Some("u")); assert!(delta.info.as_ref().unwrap().timestamp.unwrap() > 0); assert_eq!(delta.ops.len(), 1);
        let op = &delta.ops[0]; assert_eq!(op.kind.unwrap().enum_value().unwrap(), p::op::Kind::ADD);
        let add = op.add.as_ref().unwrap(); assert_eq!(add.add_last, Some(true)); assert_eq!(add.items.len(), 1); assert_eq!(add.items[0].uri.as_deref(), Some("spotify:track:1234567890123456789012"));
    }
}
