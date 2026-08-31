//! Diagnostic: read and rewrite the private Spotify rootlist, the list that
//! holds playlist folders.
//!
//!   cargo run --example rootlist_probe               # print the real hierarchy
//!   cargo run --example rootlist_probe -- --write    # run every write experiment
//!   cargo run --example rootlist_probe -- --clean    # remove anything it left
//!
//! The public Web API cannot see folders at all, so this answers what the
//! private `playlist/v2/user/{user}/rootlist` endpoint actually accepts, using
//! the librespot credentials Fastpotify already cached for local playback.
//!
//! # Safety
//!
//! Most experiments ask whether Spotify locates an operation by the items it
//! carries or by an index. An experiment that omits the index cannot be made
//! safe by checking that the items it names are disposable, because the whole
//! question is whether the server reads those names at all. An ignored `items`
//! field leaves `from_index` absent, and an absent proto2 field may read as
//! zero, so the operation would land on whatever sits at the top of the real
//! library.
//!
//! Every experiment therefore arranges the head of the rootlist to hold only
//! items the probe owns, sacrificial ones first, as a balanced span at least as
//! long as anything the experiment names. `assert_head_is_disposable` proves
//! that immediately before each mutation. An operation that falls back to index
//! zero then destroys probe data, which is exactly the observation wanted,
//! instead of a playlist.
//!
//! The two playlists the probe needs are created through the Web API with the
//! token Fastpotify already cached, and unfollowed again at the end. The run
//! finishes by comparing the whole URI vector against the one it started from.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use librespot_core::{Session, SessionConfig, cache::Cache};
use librespot_protocol::playlist4_external::{
    Add, Delta, Item, ListChanges, Mov, Op, Rem, SelectedListContent, UpdateItemUris,
    UriReplacement, op::Kind,
};
use protobuf::Message;

/// Every probe folder id contains this, so cleanup can find them and the
/// guards can tell probe markers from real ones. Spotify parses a group id as a
/// hexadecimal u64, so every character has to be a hex digit.
const PREFIX: &str = "fa57007ffee1d0";
/// Sacrificial folders: whatever a fallback coordinate hits.
const S1: &str = "fa57007ffee1d051";
const S2: &str = "fa57007ffee1d052";
/// Targets: what an identity-addressed operation should hit instead.
const P: &str = "fa57007ffee1d0aa";
const Q: &str = "fa57007ffee1d0bb";
/// A 16-digit id whose first digit is zero, to see what comes back.
const Z: &str = "0fa57007ffee1d0e";

fn start_uri(id: &str, name: &str) -> String {
    format!("spotify:start-group:{id}:{}", encode_name(name))
}

fn end_uri(id: &str) -> String {
    format!("spotify:end-group:{id}")
}

/// Spotify writes folder names into the marker URI with form encoding: spaces
/// become `+` and everything else outside the unreserved set is percent-escaped
/// UTF-8.
fn encode_name(name: &str) -> String {
    urlencoding::encode(name).replace("%20", "+")
}

fn decode_name(raw: &str) -> String {
    urlencoding::decode(&raw.replace('+', " "))
        .map(|name| name.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The id inside a marker URI, whichever end it is.
fn marker_id(uri: &str) -> Option<&str> {
    let rest = uri
        .strip_prefix("spotify:start-group:")
        .or_else(|| uri.strip_prefix("spotify:end-group:"))?;
    Some(rest.split(':').next().unwrap_or(rest))
}

fn is_probe_uri(uri: &str, playlists: &[String]) -> bool {
    match marker_id(uri) {
        Some(id) => id.to_ascii_lowercase().contains(PREFIX),
        None => playlists.iter().any(|held| held == uri),
    }
}

struct Rootlist {
    revision: Vec<u8>,
    uris: Vec<String>,
    /// The item count the server declares for the whole list, markers included.
    declared: i32,
    reads: usize,
    owner: String,
    can_edit_items: Option<bool>,
}

impl Rootlist {
    fn index_of(&self, uri: &str) -> Option<usize> {
        self.uris.iter().position(|held| held == uri)
    }

    /// The first marker URI whose id matches, whatever name or padding the
    /// server chose to store.
    fn find_marker(&self, id: &str, start: bool) -> Option<usize> {
        let wanted = u64::from_str_radix(id, 16).ok()?;
        self.uris.iter().position(|uri| {
            let opens = uri.starts_with("spotify:start-group:");
            if opens != start {
                return false;
            }
            marker_id(uri)
                .and_then(|held| u64::from_str_radix(held, 16).ok())
                .is_some_and(|held| held == wanted)
        })
    }

    fn has_folder(&self, id: &str) -> bool {
        self.find_marker(id, true).is_some()
    }

    /// One short symbol per item, so a head can be compared at a glance.
    fn head(&self, count: usize, playlists: &[String]) -> String {
        self.uris
            .iter()
            .take(count)
            .map(|uri| label(uri, playlists))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn label(uri: &str, playlists: &[String]) -> String {
    let name = |id: &str| match u64::from_str_radix(id, 16) {
        Ok(value) if value == u64::from_str_radix(S1, 16).unwrap() => "S1".to_string(),
        Ok(value) if value == u64::from_str_radix(S2, 16).unwrap() => "S2".to_string(),
        Ok(value) if value == u64::from_str_radix(P, 16).unwrap() => "P".to_string(),
        Ok(value) if value == u64::from_str_radix(Q, 16).unwrap() => "Q".to_string(),
        Ok(value) if value == u64::from_str_radix(Z, 16).unwrap() => "Z".to_string(),
        _ => format!("?{id}"),
    };
    if let Some(rest) = uri.strip_prefix("spotify:start-group:") {
        return format!("{}<", name(rest.split(':').next().unwrap_or(rest)));
    }
    if let Some(rest) = uri.strip_prefix("spotify:end-group:") {
        return format!(">{}", name(rest));
    }
    match playlists.iter().position(|held| held == uri) {
        Some(index) => format!("X{}", index + 1),
        None => ".".to_string(),
    }
}

struct Ctx {
    session: Session,
    user: String,
    http: reqwest::Client,
    /// The Web API access token Fastpotify cached. The probe never refreshes
    /// it: a refresh would rotate the token the running app still holds.
    token: String,
    /// The disposable playlists, in creation order.
    playlists: Vec<String>,
}

impl Ctx {
    fn playlist_id(&self, index: usize) -> &str {
        self.playlists[index]
            .rsplit(':')
            .next()
            .expect("a playlist uri")
    }
}

/// Reads the whole rootlist. The server honours a large `length`, so this is
/// normally one request; the loop is there for a library big enough to be
/// truncated anyway.
async fn read(ctx: &Ctx) -> Result<Rootlist> {
    const PAGE: usize = 2000;
    let mut uris: Vec<String> = Vec::new();
    let mut revision = Vec::new();
    let mut declared = 0;
    let mut reads = 0;
    let mut owner = String::new();
    let mut can_edit_items = None;
    loop {
        let bytes = ctx
            .session
            .spclient()
            .get_rootlist(uris.len(), Some(PAGE))
            .await
            .map_err(|error| anyhow!("rootlist GET: {error}"))?;
        let page = SelectedListContent::parse_from_bytes(&bytes)?;
        reads += 1;
        let expected_pos = i32::try_from(uris.len()).context("rootlist is too large")?;
        if reads == 1 {
            if page.length() < 0 {
                bail!("the server declared a negative rootlist length");
            }
            if page.revision().is_empty() {
                bail!("the server returned an empty rootlist revision");
            }
            revision = page.revision().to_vec();
            declared = page.length();
            owner = page.owner_username().to_string();
            can_edit_items = page
                .capabilities
                .as_ref()
                .and_then(|caps| caps.can_edit_items);
        } else if page.revision() != revision.as_slice() {
            bail!("the revision changed while reading; start again");
        } else if page.length() != declared {
            bail!("the declared length changed while reading; start again");
        }
        let Some(contents) = page.contents.as_ref() else {
            // An account with no playlists at all has nothing to page through.
            if declared == 0 {
                break;
            }
            bail!("rootlist page {reads} declared {declared} items but carried no contents");
        };
        if contents.pos() != expected_pos {
            bail!(
                "rootlist page {reads} starts at {}, expected {expected_pos}",
                contents.pos()
            );
        }
        if contents.truncated() && contents.items.is_empty() {
            bail!("rootlist page {reads} is truncated but made no progress");
        }
        for item in &contents.items {
            uris.push(item.uri().to_string());
        }
        if !contents.truncated() {
            break;
        }
    }
    if uris.len() != declared as usize {
        bail!(
            "assembled {} rootlist items, but the server declared {declared}",
            uris.len()
        );
    }
    Ok(Rootlist {
        revision,
        uris,
        declared,
        reads,
        owner,
        can_edit_items,
    })
}

fn print_tree(list: &Rootlist) {
    println!(
        "\n=== rootlist: {} items, {} declared, {} request(s), revision {} ===",
        list.uris.len(),
        list.declared,
        list.reads,
        hex(&list.revision)
    );
    println!(
        "owner_username {:?}, capabilities.can_edit_items {:?}",
        list.owner, list.can_edit_items
    );
    let mut depth = 0usize;
    let mut folders = 0usize;
    for (index, uri) in list.uris.iter().enumerate() {
        if let Some(rest) = uri.strip_prefix("spotify:start-group:") {
            let (id, name) = rest.split_once(':').unwrap_or((rest, ""));
            println!(
                "{index:>4}  {:indent$}[{id}] {}   raw {name:?}",
                "",
                decode_name(name),
                indent = depth * 2
            );
            depth += 1;
            folders += 1;
        } else if uri.starts_with("spotify:end-group:") {
            depth = depth.saturating_sub(1);
            println!("{index:>4}  {:indent$}/", "", indent = depth * 2);
        } else {
            println!("{index:>4}  {:indent$}{uri}", "", indent = depth * 2);
        }
    }
    let kinds: BTreeSet<_> = list
        .uris
        .iter()
        .map(|uri| uri.split(':').nth(1).unwrap_or("?"))
        .collect();
    println!("{folders} folders, depth back to {depth}, item kinds {kinds:?}");
}

// --- operations ------------------------------------------------------------

fn item(uri: &str) -> Item {
    let mut item = Item::new();
    item.set_uri(uri.to_string());
    item
}

fn op(kind: Kind) -> Op {
    let mut op = Op::new();
    op.set_kind(kind);
    op
}

fn wrap_add(add: Add) -> Op {
    let mut o = op(Kind::ADD);
    o.add = Some(add).into();
    o
}

fn wrap_rem(rem: Rem) -> Op {
    let mut o = op(Kind::REM);
    o.rem = Some(rem).into();
    o
}

fn wrap_mov(mov: Mov) -> Op {
    let mut o = op(Kind::MOV);
    o.mov = Some(mov).into();
    o
}

fn items_of(uris: &[String]) -> Vec<Item> {
    uris.iter().map(|uri| item(uri)).collect()
}

/// ADD by index, the form the first probe established.
fn add_at(at: usize, uris: &[String]) -> Op {
    let mut add = Add::new();
    add.set_from_index(at as i32);
    add.items = items_of(uris);
    wrap_add(add)
}

fn rem_at(at: usize, length: usize) -> Op {
    let mut rem = Rem::new();
    rem.set_from_index(at as i32);
    rem.set_length(length as i32);
    wrap_rem(rem)
}

fn mov_at(from: usize, length: usize, to: usize) -> Op {
    let mut mov = Mov::new();
    mov.set_from_index(from as i32);
    mov.set_length(length as i32);
    mov.set_to_index(to as i32);
    wrap_mov(mov)
}

fn folder(id: &str, name: &str) -> [String; 2] {
    [start_uri(id, name), end_uri(id)]
}

// --- transport -------------------------------------------------------------

/// What one POST told us. The probe speaks to the endpoint with reqwest rather
/// than through `SpClient`, for three reasons: `SpClient` retries a failed
/// request up to ten times, which would apply an ADD twice; its HTTP client
/// sleeps and repeats on 429; and both hide the status code and the response
/// body, which are half of what this probe is trying to record.
struct Sent {
    status: u16,
    reply: Option<SelectedListContent>,
    body: Vec<u8>,
}

impl Sent {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn verdict(&self) -> String {
        if self.ok() {
            "accepted".to_string()
        } else {
            format!("rejected {}", self.status)
        }
    }

    /// Everything the reply carries that a client could synchronise from.
    fn details(&self) -> String {
        let Some(reply) = &self.reply else {
            return format!("{} bytes, unparsed", self.body.len());
        };
        let mut parts = vec![format!("revision {}", hex(reply.revision()))];
        if !reply.resulting_revisions.is_empty() {
            parts.push(format!(
                "resulting_revisions [{}]",
                reply
                    .resulting_revisions
                    .iter()
                    .map(|revision| hex(revision))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(diff) = reply.sync_result.as_ref() {
            parts.push(format!(
                "sync_result {} op(s) {}..{}",
                diff.ops.len(),
                hex(diff.from_revision()),
                hex(diff.to_revision())
            ));
        }
        if let Some(contents) = reply.contents.as_ref() {
            parts.push(format!("contents {} item(s)", contents.items.len()));
        }
        for (name, value) in [
            ("multiple_heads", reply.multiple_heads),
            ("up_to_date", reply.up_to_date),
            ("changes_require_resync", reply.changes_require_resync),
        ] {
            if let Some(value) = value {
                parts.push(format!("{name} {value}"));
            }
        }
        if !reply.nonces.is_empty() {
            parts.push(format!("nonces {:?}", reply.nonces));
        }
        parts.join(", ")
    }
}

fn changes(base: Option<&[u8]>, ops: Vec<Op>, nonce: Option<i64>) -> ListChanges {
    let mut delta = Delta::new();
    delta.ops = ops;
    let mut changes = ListChanges::new();
    if let Some(base) = base {
        changes.set_base_revision(base.to_vec());
    }
    changes.deltas.push(delta);
    // Both flags are what Spotify's own web client sends, and they are the only
    // way to see what the server decided it applied.
    changes.set_want_resulting_revisions(true);
    changes.set_want_sync_result(true);
    if let Some(nonce) = nonce {
        changes.nonces.push(nonce);
    }
    changes
}

/// Sends a mutation exactly once, as protobuf or as the JSON the web client
/// uses. Nothing here retries: a lost response after Spotify applied an ADD
/// would create the folder twice.
async fn send(ctx: &Ctx, changes: &ListChanges, json: bool) -> Result<Sent> {
    let base = ctx.session.spclient().base_url().await?;
    let url = format!(
        "{base}/playlist/v2/user/{}/rootlist/changes?product=0&country={}&salt={}",
        ctx.user,
        ctx.session.country(),
        rand::random::<u32>()
    );
    let token = ctx.session.login5().auth_token().await?;
    let (content_type, body) = if json {
        let options = protobuf_json_mapping::PrintOptions {
            // The captured web-client request carries `"kind":4`, not `"MOV"`.
            enum_values_int: true,
            ..Default::default()
        };
        let text = protobuf_json_mapping::print_to_string_with_options(changes, &options)?;
        ("application/json", text.into_bytes())
    } else {
        ("application/x-protobuf", changes.write_to_bytes()?)
    };
    let mut request = ctx
        .http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("{} {}", token.token_type, token.access_token),
        );
    if let Ok(client_token) = ctx.session.spclient().client_token().await {
        request = request.header("client-token", client_token);
    }
    let response = request.body(body).send().await?;
    let status = response.status().as_u16();
    let body = response.bytes().await?.to_vec();
    let reply = SelectedListContent::parse_from_bytes(&body).ok();
    Ok(Sent {
        status,
        reply,
        body,
    })
}

/// One change, sent once, reported in one line.
async fn post(ctx: &Ctx, base: Option<&[u8]>, ops: Vec<Op>) -> Result<Sent> {
    let sent = send(ctx, &changes(base, ops, None), false).await?;
    println!("      POST {} ({})", sent.verdict(), sent.details());
    Ok(sent)
}

// --- the Web API side ------------------------------------------------------

async fn create_playlist(ctx: &Ctx, name: &str) -> Result<String> {
    let response = ctx
        .http
        // `/v1/users/{id}/playlists` answers 403 for this client, the same
        // way it does for Fastpotify, which posts to `/me/playlists` too.
        .post("https://api.spotify.com/v1/me/playlists")
        .bearer_auth(&ctx.token)
        .json(&serde_json::json!({
            "name": name,
            "public": false,
            "description": "Disposable item made by fastpotify's rootlist probe.",
        }))
        .send()
        .await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await?;
    if !status.is_success() {
        bail!("creating {name}: {status} {body}");
    }
    body.get("uri")
        .and_then(|uri| uri.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("the create response carried no uri: {body}"))
}

/// Whether the playlist is still in the account's library. The obvious
/// endpoint for this, `/playlists/{id}/followers/contains`, answers 403 for
/// this client, so the check is the same list Fastpotify's sidebar reads.
async fn follows(ctx: &Ctx, playlist_id: &str) -> Result<bool> {
    let wanted = format!("spotify:playlist:{playlist_id}");
    let mut url = "https://api.spotify.com/v1/me/playlists?limit=50".to_string();
    loop {
        let response = ctx.http.get(&url).bearer_auth(&ctx.token).send().await?;
        let status = response.status();
        let body: serde_json::Value = response.json().await?;
        if !status.is_success() {
            bail!("listing the library: {status} {body}");
        }
        let items = body
            .get("items")
            .and_then(|items| items.as_array())
            .ok_or_else(|| anyhow!("no items in the library page: {body}"))?;
        if items
            .iter()
            .filter_map(|item| item.get("uri").and_then(|uri| uri.as_str()))
            .any(|uri| uri == wanted)
        {
            return Ok(true);
        }
        match body.get("next").and_then(|next| next.as_str()) {
            Some(next) => url = next.to_string(),
            None => return Ok(false),
        }
    }
}

async fn unfollow(ctx: &Ctx, playlist_id: &str) -> Result<()> {
    let response = ctx
        .http
        .delete(format!(
            "https://api.spotify.com/v1/playlists/{playlist_id}/followers"
        ))
        .bearer_auth(&ctx.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("unfollowing {playlist_id}: {}", response.status());
    }
    Ok(())
}

// --- arranging a safe head -------------------------------------------------

/// What the head of the rootlist should hold while an experiment runs.
#[derive(Clone, Copy)]
enum Slot {
    /// A folder the probe creates, empty unless a child follows it.
    Folder(&'static str, &'static str),
    /// A folder that closes after the slots nested inside it.
    Open(&'static str, &'static str),
    Close,
    /// One of the disposable playlists, by creation order.
    Playlist(usize),
}

/// Removes every folder the probe ever created, in balanced marker pairs.
async fn clean(ctx: &Ctx) -> Result<()> {
    loop {
        let list = read(ctx).await?;
        let Some(start) = list
            .uris
            .iter()
            .position(|uri| uri.starts_with("spotify:start-group:") && is_probe_uri(uri, &[]))
        else {
            return Ok(());
        };
        let id = marker_id(&list.uris[start]).unwrap_or_default().to_string();
        let end = list
            .index_of(&end_uri(&id))
            .ok_or_else(|| anyhow!("probe folder {id} has no end marker"))?;
        if !is_probe_uri(&list.uris[start], &[]) || !is_probe_uri(&list.uris[end], &[]) {
            bail!("refusing to remove an index that is not a probe marker");
        }
        // The server refuses a delta that leaves the markers unbalanced, so
        // both go in one request, the later index first. Anything the folder
        // held stays in the list, one level further out.
        let sent = post(
            ctx,
            Some(&list.revision),
            vec![rem_at(end, 1), rem_at(start, 1)],
        )
        .await?;
        if !sent.ok() {
            bail!("cleanup {}", sent.verdict());
        }
    }
}

/// Builds the head of the rootlist out of probe-owned items, one request per
/// slot so every index is taken from a fresh snapshot. Slots land in order, so
/// slot zero is whatever a fallback coordinate would destroy.
async fn arrange(ctx: &Ctx, slots: &[Slot]) -> Result<Rootlist> {
    clean(ctx).await?;
    let mut placed = 0usize;
    for slot in slots {
        let list = read(ctx).await?;
        match *slot {
            Slot::Folder(id, name) => {
                let sent = post(
                    ctx,
                    Some(&list.revision),
                    vec![add_at(placed, &folder(id, name))],
                )
                .await?;
                if !sent.ok() {
                    bail!("arranging folder {id}: {}", sent.verdict());
                }
                placed += 2;
            }
            Slot::Open(id, name) => {
                let sent = post(
                    ctx,
                    Some(&list.revision),
                    vec![add_at(placed, &folder(id, name))],
                )
                .await?;
                if !sent.ok() {
                    bail!("arranging folder {id}: {}", sent.verdict());
                }
                placed += 1;
            }
            Slot::Close => {
                // The end marker is already in place from `Open`; the slots in
                // between were inserted before it.
                placed += 1;
            }
            Slot::Playlist(which) => {
                let uri = ctx.playlists[which].clone();
                let from = list
                    .index_of(&uri)
                    .ok_or_else(|| anyhow!("disposable playlist {uri} is not in the rootlist"))?;
                if from != placed {
                    let sent =
                        post(ctx, Some(&list.revision), vec![mov_at(from, 1, placed)]).await?;
                    if !sent.ok() {
                        bail!("arranging playlist {uri}: {}", sent.verdict());
                    }
                }
                placed += 1;
            }
        }
    }
    let list = read(ctx).await?;
    assert_head_is_disposable(ctx, &list, placed)?;
    println!(
        "    head {} | {placed} probe items",
        list.head(placed, &ctx.playlists)
    );
    Ok(list)
}

/// The guard every experiment leans on: nothing an operation can reach by
/// falling back to the front of the list belongs to the real library.
fn assert_head_is_disposable(ctx: &Ctx, list: &Rootlist, count: usize) -> Result<()> {
    for index in 0..count {
        let uri = list
            .uris
            .get(index)
            .ok_or_else(|| anyhow!("the rootlist is shorter than the arranged head"))?;
        if !is_probe_uri(uri, &ctx.playlists) {
            bail!("item {index} is {uri}, which the probe does not own; refusing to write");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let arg = |name: &str| std::env::args().any(|value| value == name);

    let state = directories::ProjectDirs::from("me", "paolino", "fastpotify")
        .map(|dirs| {
            dirs.state_dir()
                .map(PathBuf::from)
                .unwrap_or_else(|| dirs.data_local_dir().to_path_buf())
        })
        .ok_or_else(|| anyhow!("no home directory"))?;
    let credentials_dir = state.join("credentials");
    let cache = Cache::new(Some(&credentials_dir), None, None, None)?;
    let credentials = cache.credentials().ok_or_else(|| {
        anyhow!(
            "no cached credentials in {}; sign in to Fastpotify first",
            credentials_dir.display()
        )
    })?;
    let session = Session::new(SessionConfig::default(), Some(cache));
    session
        .connect(credentials, false)
        .await
        .context("connecting the private session")?;
    let user = session.username();
    println!("session.username() = {user}   (the account id the Web API also reports)");

    let token = if arg("--write") {
        read_web_token(&state)?
    } else {
        String::new()
    };
    let mut ctx = Ctx {
        session,
        user: user.clone(),
        http: reqwest::Client::new(),
        token,
        playlists: Vec::new(),
    };

    let baseline = read(&ctx).await?;

    if arg("--clean") {
        clean(&ctx).await?;
        println!("clean");
        return Ok(());
    }
    if !arg("--write") {
        print_tree(&baseline);
        println!("\n(read only; pass --write for the experiments)");
        return Ok(());
    }

    println!("\n### disposable playlists");
    for name in ["fastpotify probe 1", "fastpotify probe 2"] {
        let uri = create_playlist(&ctx, name).await?;
        let list = read(&ctx).await?;
        println!(
            "  created {uri}; the rootlist now holds it at {:?}",
            list.index_of(&uri)
        );
        ctx.playlists.push(uri);
    }

    let outcome = experiments(&ctx).await;

    println!("\n### teardown");
    clean(&ctx).await?;
    for index in 0..ctx.playlists.len() {
        let id = ctx.playlist_id(index).to_string();
        match unfollow(&ctx, &id).await {
            Ok(()) => println!("  unfollowed {}", ctx.playlists[index]),
            Err(error) => println!("  could not unfollow {}: {error}", ctx.playlists[index]),
        }
    }
    let after = read(&ctx).await?;
    println!(
        "\n=== library restored: {} ===",
        if after.uris == baseline.uris {
            "yes".to_string()
        } else {
            format!(
                "NO, {} items against {}",
                after.uris.len(),
                baseline.uris.len()
            )
        }
    );
    outcome
}

fn read_web_token(state: &std::path::Path) -> Result<String> {
    let path = state.join("personal_web_api_token.json");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let expires_at = value
        .get("expires_at")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    if expires_at <= now {
        bail!(
            "the cached Web API token expired {} seconds ago; open Fastpotify so it refreshes one \
             (the probe must not refresh it itself, that would rotate the token the app holds)",
            now - expires_at
        );
    }
    value
        .get("access_token")
        .and_then(|token| token.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no access_token in {}", path.display()))
}

async fn experiments(ctx: &Ctx) -> Result<()> {
    // `--only <name>` reruns one section after a fix.
    let mut args = std::env::args().skip_while(|arg| arg != "--only");
    if args.next().is_some() {
        return match args.next().as_deref() {
            Some("playlists") => playlist_semantics(ctx).await,
            Some("anchors") => move_anchors(ctx).await,
            Some("nonces") => nonces(ctx).await,
            Some("revisions") => revisions(ctx).await,
            other => bail!("no experiment section called {other:?}"),
        };
    }
    read_facts(ctx).await?;
    add_anchors(ctx).await?;
    rename_addressing(ctx).await?;
    move_addressing(ctx).await?;
    move_anchors(ctx).await?;
    remove_addressing(ctx).await?;
    revisions(ctx).await?;
    nonces(ctx).await?;
    ids_and_names(ctx).await?;
    playlist_semantics(ctx).await?;
    Ok(())
}

// --- experiments -----------------------------------------------------------

async fn read_facts(ctx: &Ctx) -> Result<()> {
    println!("\n### what one read returns");
    for wanted in [100usize, 500, 5000] {
        match ctx.session.spclient().get_rootlist(0, Some(wanted)).await {
            Ok(bytes) => {
                let page = SelectedListContent::parse_from_bytes(&bytes)?;
                println!(
                    "  asked {wanted:<5} got {:<5} declared {:<5} truncated {:?}",
                    page.contents.as_ref().map_or(0, |c| c.items.len()),
                    page.length(),
                    page.contents.as_ref().map(|c| c.truncated()),
                );
            }
            Err(error) => println!("  asked {wanted:<5} error {error}"),
        }
    }
    let list = read(ctx).await?;
    println!(
        "  owner_username {:?}, can_edit_items {:?}",
        list.owner, list.can_edit_items
    );
    Ok(())
}

async fn add_anchors(ctx: &Ctx) -> Result<()> {
    println!("\n### ADD: can the destination be an item instead of an index?");
    // ADD destroys nothing, so this is the safe place to learn whether the
    // server reads anchors at all before any destructive form is tried.
    for (name, build) in add_forms() {
        let list = arrange(ctx, &[Slot::Folder(P, "target")]).await?;
        let sent = post(ctx, Some(&list.revision), vec![build(&list)]).await?;
        let after = read(ctx).await?;
        let landed = after.find_marker(Q, true);
        let inside = landed == after.find_marker(P, true).map(|start| start + 1);
        println!(
            "  {name:<28} {}; Q at {landed:?}, first child of P {inside}, head {}",
            sent.verdict(),
            after.head(6, &ctx.playlists)
        );
    }
    Ok(())
}

/// One named way of writing an operation, built against a fresh snapshot.
type Form = (&'static str, fn(&Rootlist) -> Op);

fn add_forms() -> Vec<Form> {
    vec![
        ("from_index (control)", |list| {
            let start = list.find_marker(P, true).unwrap_or(0);
            add_at(start + 1, &folder(Q, "anchored"))
        }),
        ("add_after_item, no index", |list| {
            let mut add = Add::new();
            add.items = items_of(&folder(Q, "anchored"));
            add.add_after_item =
                Some(item(&list.uris[list.find_marker(P, true).unwrap_or(0)])).into();
            wrap_add(add)
        }),
        ("add_before_item, no index", |list| {
            let mut add = Add::new();
            add.items = items_of(&folder(Q, "anchored"));
            let end = list.find_marker(P, false).unwrap_or(0);
            add.add_before_item = Some(item(&list.uris[end])).into();
            wrap_add(add)
        }),
        ("add_first, no index", |_| {
            let mut add = Add::new();
            add.items = items_of(&folder(Q, "anchored"));
            add.set_add_first(true);
            wrap_add(add)
        }),
        ("add_last, no index", |_| {
            let mut add = Add::new();
            add.items = items_of(&folder(Q, "anchored"));
            add.set_add_last(true);
            wrap_add(add)
        }),
        ("items only, nothing else", |_| {
            let mut add = Add::new();
            add.items = items_of(&folder(Q, "anchored"));
            wrap_add(add)
        }),
    ]
}

async fn rename_addressing(ctx: &Ctx) -> Result<()> {
    println!("\n### UPDATE_ITEM_URIS: index, item, or both?");
    // S1 sits at index zero, so a form that falls back to index zero renames a
    // sacrificial marker instead of a real playlist's URI.
    let forms: Vec<Form> = vec![
        ("index and item (control)", |list| {
            let start = list.find_marker(P, true).unwrap();
            update(
                Some(start),
                Some(&list.uris[start]),
                &start_uri(P, "renamed"),
            )
        }),
        ("item only, no index", |list| {
            let start = list.find_marker(P, true).unwrap();
            update(None, Some(&list.uris[start]), &start_uri(P, "renamed"))
        }),
        ("index only, no item", |list| {
            let start = list.find_marker(P, true).unwrap();
            update(Some(start), None, &start_uri(P, "renamed"))
        }),
        ("P's item at S1's index", |list| {
            let start = list.find_marker(P, true).unwrap();
            update(Some(0), Some(&list.uris[start]), &start_uri(P, "renamed"))
        }),
    ];
    for (name, build) in forms {
        let list = arrange(
            ctx,
            &[Slot::Folder(S1, "sacrifice"), Slot::Folder(P, "target")],
        )
        .await?;
        let sacrifice_before = list.uris[0].clone();
        let sent = post(ctx, Some(&list.revision), vec![build(&list)]).await?;
        let after = read(ctx).await?;
        let renamed = after.index_of(&start_uri(P, "renamed")).is_some();
        let sacrifice_hit = after.uris.first() != Some(&sacrifice_before);
        println!(
            "  {name:<26} {}; P renamed {renamed}, sacrifice touched {sacrifice_hit}",
            sent.verdict()
        );
    }
    Ok(())
}

fn update(index: Option<usize>, from: Option<&str>, new_uri: &str) -> Op {
    let mut replacement = UriReplacement::new();
    if let Some(index) = index {
        replacement.set_index(index as i32);
    }
    if let Some(from) = from {
        replacement.item = Some(item(from)).into();
    }
    replacement.set_new_uri(new_uri.to_string());
    let mut update = UpdateItemUris::new();
    update.uri_replacements.push(replacement);
    let mut o = op(Kind::UPDATE_ITEM_URIS);
    o.update_item_uris = Some(update).into();
    o
}

async fn move_addressing(ctx: &Ctx) -> Result<()> {
    println!("\n### MOV: a playlist into a folder, by items or by index");
    // X1 is the sacrifice at index zero: one item, matching the length of the
    // move being tested. X2 is what an identity-addressed move should carry.
    for (name, json) in [("protobuf", false), ("json, the captured form", true)] {
        let list = arrange(
            ctx,
            &[
                Slot::Playlist(0),
                Slot::Playlist(1),
                Slot::Folder(P, "destination"),
            ],
        )
        .await?;
        let start = list.find_marker(P, true).unwrap();
        let mut mov = Mov::new();
        mov.items = items_of(&[ctx.playlists[1].clone()]);
        mov.add_after_item = Some(item(&list.uris[start])).into();
        let sent = send(
            ctx,
            &changes(Some(&list.revision), vec![wrap_mov(mov)], None),
            json,
        )
        .await?;
        println!("      POST {} ({})", sent.verdict(), sent.details());
        let after = read(ctx).await?;
        let inside =
            after.find_marker(P, true).map(|start| start + 1) == after.index_of(&ctx.playlists[1]);
        println!(
            "  items + add_after_item, {name:<24} {}; X2 inside P {inside}, X1 moved {}, head {}",
            sent.verdict(),
            after.index_of(&ctx.playlists[0]) != Some(0),
            after.head(6, &ctx.playlists)
        );
    }

    println!("\n### MOV: a whole folder span by its items");
    let list = arrange(
        ctx,
        &[
            Slot::Folder(S1, "sacrifice"),
            Slot::Folder(Q, "moved"),
            Slot::Folder(P, "destination"),
        ],
    )
    .await?;
    let q_start = list.find_marker(Q, true).unwrap();
    let q_end = list.find_marker(Q, false).unwrap();
    let p_start = list.find_marker(P, true).unwrap();
    let mut mov = Mov::new();
    mov.items = items_of(&[list.uris[q_start].clone(), list.uris[q_end].clone()]);
    mov.add_after_item = Some(item(&list.uris[p_start])).into();
    let sent = post(ctx, Some(&list.revision), vec![wrap_mov(mov)]).await?;
    let after = read(ctx).await?;
    println!(
        "  folder span by items: {}; head {}",
        sent.verdict(),
        after.head(6, &ctx.playlists)
    );

    println!("\n### MOV: how to_index is counted");
    for offset in [0i32, 1, -1] {
        let list = arrange(
            ctx,
            &[Slot::Folder(S1, "sacrifice"), Slot::Folder(P, "moved")],
        )
        .await?;
        let from = list.find_marker(P, true).unwrap();
        let to = (from as i32 + offset).max(0) as usize;
        let sent = post(ctx, Some(&list.revision), vec![mov_at(from, 2, to)]).await?;
        let after = read(ctx).await?;
        println!(
            "  from {from} length 2 to_index {to}: {}; head {}",
            sent.verdict(),
            after.head(6, &ctx.playlists)
        );
    }
    Ok(())
}

async fn remove_addressing(ctx: &Ctx) -> Result<()> {
    println!("\n### REM: does items_as_key make the items decide?");
    let forms: Vec<Form> = vec![
        ("index and length (control)", |list| {
            rem_at(list.find_marker(P, true).unwrap(), 2)
        }),
        ("items_as_key, index at S1", |list| {
            let start = list.find_marker(P, true).unwrap();
            let mut rem = Rem::new();
            rem.set_items_as_key(true);
            rem.set_from_index(0);
            rem.set_length(2);
            rem.items = items_of(&[list.uris[start].clone(), list.uris[start + 1].clone()]);
            wrap_rem(rem)
        }),
        ("items_as_key, no index", |list| {
            let start = list.find_marker(P, true).unwrap();
            let mut rem = Rem::new();
            rem.set_items_as_key(true);
            rem.items = items_of(&[list.uris[start].clone(), list.uris[start + 1].clone()]);
            wrap_rem(rem)
        }),
        ("items only, no flag, no index", |list| {
            let start = list.find_marker(P, true).unwrap();
            let mut rem = Rem::new();
            rem.items = items_of(&[list.uris[start].clone(), list.uris[start + 1].clone()]);
            wrap_rem(rem)
        }),
    ];
    for (name, build) in forms {
        // The sacrifice is a balanced two-item span, the same length as the
        // folder being removed, so an index-zero fallback is both survivable
        // and visible.
        let list = arrange(
            ctx,
            &[Slot::Folder(S1, "sacrifice"), Slot::Folder(P, "target")],
        )
        .await?;
        let sent = post(ctx, Some(&list.revision), vec![build(&list)]).await?;
        let after = read(ctx).await?;
        println!(
            "  {name:<30} {}; P gone {}, S1 gone {}, head {}",
            sent.verdict(),
            !after.has_folder(P),
            !after.has_folder(S1),
            after.head(6, &ctx.playlists)
        );
    }
    Ok(())
}

async fn revisions(ctx: &Ctx) -> Result<()> {
    println!("\n### base_revision");
    let list = arrange(
        ctx,
        &[Slot::Folder(S1, "sacrifice"), Slot::Folder(P, "target")],
    )
    .await?;
    let start = list.find_marker(P, true).unwrap();
    let sent = post(ctx, None, vec![rem_at(start, 2)]).await?;
    let after = read(ctx).await?;
    println!(
        "  omitted entirely, index form: {}; P gone {}",
        sent.verdict(),
        !after.has_folder(P)
    );

    let list = arrange(
        ctx,
        &[Slot::Folder(S1, "sacrifice"), Slot::Folder(P, "target")],
    )
    .await?;
    let start = list.find_marker(P, true).unwrap();
    let mut rem = Rem::new();
    rem.set_items_as_key(true);
    rem.items = items_of(&[list.uris[start].clone(), list.uris[start + 1].clone()]);
    let sent = post(ctx, None, vec![wrap_rem(rem)]).await?;
    let after = read(ctx).await?;
    println!(
        "  omitted entirely, identity form: {}; P gone {}",
        sent.verdict(),
        !after.has_folder(P)
    );

    let list = arrange(
        ctx,
        &[Slot::Folder(S1, "sacrifice"), Slot::Folder(P, "target")],
    )
    .await?;
    let start = list.find_marker(P, true).unwrap();
    let garbage = vec![0u8; list.revision.len()];
    let sent = post(ctx, Some(&garbage), vec![rem_at(start, 2)]).await?;
    println!("  a revision the server never issued: {}", sent.verdict());

    // A stale revision with an index taken from a newer snapshot. Both the
    // stale and the fresh index have to point inside probe territory for this
    // to be safe, which the arrangement and the guard below guarantee.
    let list = arrange(
        ctx,
        &[
            Slot::Folder(S1, "sacrifice"),
            Slot::Folder(P, "first"),
            Slot::Folder(Q, "second"),
        ],
    )
    .await?;
    let stale = list.revision.clone();
    let p_start = list.find_marker(P, true).unwrap();
    let q_start = list.find_marker(Q, true).unwrap();
    let sent = post(ctx, Some(&list.revision), vec![mov_at(q_start, 2, p_start)]).await?;
    if !sent.ok() {
        bail!("could not swap the two probe folders: {}", sent.verdict());
    }
    let swapped = read(ctx).await?;
    let fresh = swapped.find_marker(P, true).unwrap();
    assert_head_is_disposable(ctx, &swapped, 6)?;
    if fresh + 1 >= 6 {
        bail!("the swapped layout left probe territory");
    }
    let sent = post(ctx, Some(&stale), vec![rem_at(fresh, 2)]).await?;
    let after = read(ctx).await?;
    println!(
        "  fresh index {fresh} with a stale revision: {}; P gone {}, Q gone {}, head {}",
        sent.verdict(),
        !after.has_folder(P),
        !after.has_folder(Q),
        after.head(6, &ctx.playlists)
    );
    Ok(())
}

async fn nonces(ctx: &Ctx) -> Result<()> {
    println!("\n### nonces: does Spotify drop a repeated change?");
    // The control first: the same ADD twice with no nonce at all. If Spotify
    // refuses the second one by itself, a deduplicated nonce would prove
    // nothing.
    let list = arrange(ctx, &[Slot::Folder(S1, "sacrifice")]).await?;
    let body = changes(
        Some(&list.revision),
        vec![add_at(0, &folder(P, "twice"))],
        None,
    );
    let first = send(ctx, &body, false).await?;
    let second = send(ctx, &body, false).await?;
    let after = read(ctx).await?;
    println!(
        "  no nonce, sent twice: {} then {}; P present {}, start markers {}",
        first.verdict(),
        second.verdict(),
        after.has_folder(P),
        after
            .uris
            .iter()
            .filter(|uri| uri.starts_with(&format!("spotify:start-group:{P}")))
            .count()
    );

    let list = arrange(ctx, &[Slot::Folder(S1, "sacrifice")]).await?;
    let nonce = rand::random::<i32>() as i64;
    let body = changes(
        Some(&list.revision),
        vec![add_at(0, &folder(P, "twice"))],
        Some(nonce),
    );
    let first = send(ctx, &body, false).await?;
    let second = send(ctx, &body, false).await?;
    let after = read(ctx).await?;
    let copies = after
        .uris
        .iter()
        .filter(|uri| uri.starts_with(&format!("spotify:start-group:{P}")))
        .count();
    println!(
        "  nonce {nonce}, sent twice: {} then {}; start markers for P {copies}",
        first.verdict(),
        second.verdict()
    );
    println!("    first reply  {}", first.details());
    println!("    second reply {}", second.details());

    // And a replay after the list moved on, which is the case a lost response
    // would produce in the wild.
    let list = arrange(ctx, &[Slot::Folder(S1, "sacrifice")]).await?;
    let nonce = rand::random::<i32>() as i64;
    let body = changes(
        Some(&list.revision),
        vec![add_at(0, &folder(P, "replay"))],
        Some(nonce),
    );
    let first = send(ctx, &body, false).await?;
    let between = read(ctx).await?;
    let sent = post(
        ctx,
        Some(&between.revision),
        vec![add_at(0, &folder(Q, "between"))],
    )
    .await?;
    if !sent.ok() {
        bail!("could not make an unrelated change: {}", sent.verdict());
    }
    let replay = send(ctx, &body, false).await?;
    let after = read(ctx).await?;
    let copies = after
        .uris
        .iter()
        .filter(|uri| uri.starts_with(&format!("spotify:start-group:{P}")))
        .count();
    println!(
        "  nonce replayed after another change: {} then {}; start markers for P {copies}",
        first.verdict(),
        replay.verdict()
    );
    Ok(())
}

async fn ids_and_names(ctx: &Ctx) -> Result<()> {
    println!("\n### which folder ids the server accepts");
    for candidate in [
        "fa57007ffee1d0cc",
        "FA57007FFEE1D0CD",
        "fa57007ffee1d0c",
        "fa57007ffee1d0ccc",
        "fa57007ffee1d0-x",
    ] {
        let list = arrange(ctx, &[Slot::Folder(S1, "sacrifice")]).await?;
        let sent = post(
            ctx,
            Some(&list.revision),
            vec![add_at(0, &folder(candidate, "id test"))],
        )
        .await?;
        let after = read(ctx).await?;
        let stored = after
            .uris
            .iter()
            .find(|uri| {
                uri.starts_with("spotify:start-group:")
                    && marker_id(uri).is_some_and(|id| {
                        id.eq_ignore_ascii_case(candidate)
                            || u64::from_str_radix(id, 16).ok()
                                == u64::from_str_radix(candidate, 16).ok()
                    })
            })
            .cloned();
        println!(
            "  {candidate:<18} {}; stored as {:?}",
            sent.verdict(),
            stored.as_deref().and_then(marker_id)
        );
    }

    println!("\n### a 16-digit id whose first digit is zero");
    let list = arrange(ctx, &[Slot::Folder(S1, "sacrifice")]).await?;
    let sent = post(
        ctx,
        Some(&list.revision),
        vec![add_at(0, &folder(Z, "zero"))],
    )
    .await?;
    let after = read(ctx).await?;
    let stored = after
        .find_marker(Z, true)
        .map(|index| after.uris[index].clone());
    println!(
        "  wrote {:?}: {}; read back {:?}, exact string survives {}",
        start_uri(Z, "zero"),
        sent.verdict(),
        stored,
        stored.as_deref() == Some(start_uri(Z, "zero").as_str())
    );
    if let Some(stored) = stored {
        // Renaming through the URI the server returned is what production
        // would do, so prove that round trip too.
        let list = read(ctx).await?;
        let index = list.index_of(&stored).unwrap();
        let sent = post(
            ctx,
            Some(&list.revision),
            vec![update(
                Some(index),
                Some(&stored),
                &start_uri(Z, "zero renamed"),
            )],
        )
        .await?;
        let after = read(ctx).await?;
        println!(
            "  renaming it through the returned URI: {}; renamed {}",
            sent.verdict(),
            after
                .find_marker(Z, true)
                .map(|index| after.uris[index].contains("renamed"))
                .unwrap_or(false)
        );
    }

    println!("\n### how long a folder name may be");
    for length in [100usize, 200, 300, 500, 1000] {
        let list = arrange(ctx, &[Slot::Folder(S1, "sacrifice")]).await?;
        let name = "n".repeat(length);
        let sent = post(
            ctx,
            Some(&list.revision),
            vec![add_at(0, &folder(P, &name))],
        )
        .await?;
        let after = read(ctx).await?;
        let stored = after
            .find_marker(P, true)
            .and_then(|index| after.uris[index].split(':').nth(3).map(str::to_string));
        println!(
            "  {length:>4} characters: {}; stored {} characters",
            sent.verdict(),
            stored.map_or("none".to_string(), |name| decode_name(&name)
                .chars()
                .count()
                .to_string())
        );
    }

    println!("\n### unicode, spaces and punctuation in a folder name");
    let name = "probe ünïcode +plus %pct : colon";
    let list = arrange(ctx, &[Slot::Folder(S1, "sacrifice")]).await?;
    let sent = post(ctx, Some(&list.revision), vec![add_at(0, &folder(P, name))]).await?;
    let after = read(ctx).await?;
    let stored = after
        .find_marker(P, true)
        .map(|index| after.uris[index].clone());
    println!(
        "  {}; round trip {:?}",
        sent.verdict(),
        stored
            .as_deref()
            .and_then(|uri| uri.splitn(4, ':').nth(3))
            .map(decode_name)
    );
    Ok(())
}

async fn playlist_semantics(ctx: &Ctx) -> Result<()> {
    println!("\n### what removing a playlist from the rootlist does to the library");
    // A folder holding one disposable playlist, removed as one span, which is
    // what "delete folder and its contents" would send.
    let list = arrange(
        ctx,
        &[
            Slot::Folder(S1, "sacrifice"),
            Slot::Open(P, "with contents"),
            Slot::Playlist(1),
            Slot::Close,
        ],
    )
    .await?;
    let start = list.find_marker(P, true).unwrap();
    let span = &list.uris[start..start + 3];
    if span[1] != ctx.playlists[1] || !span[2].starts_with("spotify:end-group:") {
        bail!("the folder did not come out holding exactly the disposable playlist: {span:?}");
    }
    let id = ctx.playlist_id(1).to_string();
    println!("  before: followed {}", follows(ctx, &id).await?);
    let sent = post(ctx, Some(&list.revision), vec![rem_at(start, 3)]).await?;
    let after = read(ctx).await?;
    println!(
        "  removing the span: {}; still in the rootlist {}, still followed {}",
        sent.verdict(),
        after.index_of(&ctx.playlists[1]).is_some(),
        follows(ctx, &id).await?
    );

    println!("\n### does a Web API unfollow reach the rootlist on its own?");
    let id = ctx.playlist_id(0).to_string();
    unfollow(ctx, &id).await?;
    let after = read(ctx).await?;
    println!(
        "  unfollowed X1; still in the rootlist {}, followed {}",
        after.index_of(&ctx.playlists[0]).is_some(),
        follows(ctx, &id).await?
    );
    Ok(())
}

/// The rest of the anchors, one per gesture the sidebar has to express: drop
/// before a sibling, drop at the end of the root, drop at the top, and drop a
/// whole folder. Every form here names its destination by item, so none of
/// them carries an index or a revision.
async fn move_anchors(ctx: &Ctx) -> Result<()> {
    println!("\n### MOV: the remaining destinations, all by item");

    let list = arrange(
        ctx,
        &[
            Slot::Playlist(0),
            Slot::Folder(Q, "sibling"),
            Slot::Playlist(1),
        ],
    )
    .await?;
    let q_start = list.find_marker(Q, true).unwrap();
    let mut mov = Mov::new();
    mov.items = items_of(&[ctx.playlists[1].clone()]);
    mov.add_before_item = Some(item(&list.uris[q_start])).into();
    let sent = post(ctx, None, vec![wrap_mov(mov)]).await?;
    let after = read(ctx).await?;
    println!(
        "  add_before_item, a folder's start marker: {}; X2 at {:?}, Q at {:?}, head {}",
        sent.verdict(),
        after.index_of(&ctx.playlists[1]),
        after.find_marker(Q, true),
        after.head(6, &ctx.playlists)
    );

    let list = arrange(ctx, &[Slot::Playlist(0), Slot::Playlist(1)]).await?;
    let mut mov = Mov::new();
    mov.items = items_of(&[ctx.playlists[1].clone()]);
    mov.set_add_last(true);
    let sent = post(ctx, None, vec![wrap_mov(mov)]).await?;
    let after = read(ctx).await?;
    println!(
        "  add_last: {}; X2 at {:?} of {} items",
        sent.verdict(),
        after.index_of(&ctx.playlists[1]),
        after.uris.len()
    );
    let _ = list;

    let list = arrange(ctx, &[Slot::Playlist(0), Slot::Playlist(1)]).await?;
    let mut mov = Mov::new();
    mov.items = items_of(&[ctx.playlists[1].clone()]);
    mov.set_add_first(true);
    let sent = post(ctx, None, vec![wrap_mov(mov)]).await?;
    let after = read(ctx).await?;
    println!(
        "  add_first: {}; X2 at {:?}",
        sent.verdict(),
        after.index_of(&ctx.playlists[1])
    );
    let _ = list;

    let list = arrange(
        ctx,
        &[
            Slot::Open(P, "moved"),
            Slot::Playlist(1),
            Slot::Close,
            Slot::Playlist(0),
        ],
    )
    .await?;
    let p_start = list.find_marker(P, true).unwrap();
    let p_end = list.find_marker(P, false).unwrap();
    let mut mov = Mov::new();
    mov.items = items_of(&[
        list.uris[p_start].clone(),
        list.uris[p_start + 1].clone(),
        list.uris[p_end].clone(),
    ]);
    mov.set_add_last(true);
    let sent = post(ctx, None, vec![wrap_mov(mov)]).await?;
    let after = read(ctx).await?;
    println!(
        "  a folder with a child, whole span, add_last: {}; P at {:?} of {} items, child follows {}",
        sent.verdict(),
        after.find_marker(P, true),
        after.uris.len(),
        after.find_marker(P, true).map(|start| start + 1) == after.index_of(&ctx.playlists[1])
    );

    println!("\n### REM: dropping a folder's markers while its contents stay");
    let list = arrange(
        ctx,
        &[
            Slot::Open(P, "emptied"),
            Slot::Playlist(1),
            Slot::Close,
            Slot::Playlist(0),
        ],
    )
    .await?;
    let p_start = list.find_marker(P, true).unwrap();
    let p_end = list.find_marker(P, false).unwrap();
    let mut rem = Rem::new();
    rem.set_items_as_key(true);
    rem.items = items_of(&[list.uris[p_start].clone(), list.uris[p_end].clone()]);
    let sent = post(ctx, None, vec![wrap_rem(rem)]).await?;
    let after = read(ctx).await?;
    println!(
        "  both markers in one op, contents between them: {}; P gone {}, X2 still there {}, head {}",
        sent.verdict(),
        !after.has_folder(P),
        after.index_of(&ctx.playlists[1]).is_some(),
        after.head(6, &ctx.playlists)
    );

    let list = arrange(
        ctx,
        &[
            Slot::Open(P, "destination"),
            Slot::Playlist(0),
            Slot::Close,
            Slot::Playlist(1),
        ],
    )
    .await?;
    let p_end = list.find_marker(P, false).unwrap();
    let mut mov = Mov::new();
    mov.items = items_of(&[ctx.playlists[1].clone()]);
    mov.add_before_item = Some(item(&list.uris[p_end])).into();
    let sent = post(ctx, None, vec![wrap_mov(mov)]).await?;
    let after = read(ctx).await?;
    println!(
        "  add_before_item, a folder's end marker: {}; X2 last inside P {}, head {}",
        sent.verdict(),
        after.find_marker(P, false).map(|end| end - 1) == after.index_of(&ctx.playlists[1]),
        after.head(6, &ctx.playlists)
    );

    println!("\n### REM: a whole folder span by items, the destructive delete");
    let list = arrange(
        ctx,
        &[
            Slot::Open(P, "with contents"),
            Slot::Playlist(1),
            Slot::Close,
            Slot::Playlist(0),
        ],
    )
    .await?;
    let p_start = list.find_marker(P, true).unwrap();
    let p_end = list.find_marker(P, false).unwrap();
    let span: Vec<String> = list.uris[p_start..=p_end].to_vec();
    let id = ctx.playlist_id(1).to_string();
    let mut rem = Rem::new();
    rem.set_items_as_key(true);
    rem.items = items_of(&span);
    let sent = post(ctx, None, vec![wrap_rem(rem)]).await?;
    let after = read(ctx).await?;
    println!(
        "  the whole span in one op: {}; P gone {}, X2 in the rootlist {}, X2 in the library {}",
        sent.verdict(),
        !after.has_folder(P),
        after.index_of(&ctx.playlists[1]).is_some(),
        follows(ctx, &id).await?
    );

    println!("\n### naming an item that is no longer there");
    let list = arrange(ctx, &[Slot::Folder(P, "vanishing")]).await?;
    let p_start = list.find_marker(P, true).unwrap();
    let stale = list.uris[p_start].clone();
    let stale_end = list.uris[p_start + 1].clone();
    let mut rem = Rem::new();
    rem.set_items_as_key(true);
    rem.items = items_of(&[stale.clone(), stale_end.clone()]);
    let sent = post(ctx, None, vec![wrap_rem(rem)]).await?;
    if !sent.ok() {
        bail!("could not remove the folder first: {}", sent.verdict());
    }
    let mut rem = Rem::new();
    rem.set_items_as_key(true);
    rem.items = items_of(&[stale, stale_end]);
    let sent = post(ctx, None, vec![wrap_rem(rem)]).await?;
    println!("  removing it a second time: {}", sent.verdict());

    let list = read(ctx).await?;
    let mut mov = Mov::new();
    mov.items = items_of(&[start_uri(Q, "never existed"), end_uri(Q)]);
    mov.set_add_last(true);
    let sent = post(ctx, None, vec![wrap_mov(mov)]).await?;
    println!("  moving a folder that never existed: {}", sent.verdict());
    let _ = list;
    Ok(())
}
