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
//! Safety: every write appends two empty folders of its own at the end of the
//! root list and only ever names indexes that hold one of their four marker
//! items. Nothing in the real library is read-modified, and `--write` ends by
//! comparing the whole list against the one it started from.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use librespot_core::{Session, SessionConfig, cache::Cache};
use librespot_protocol::playlist4_external::{
    Add, Delta, Item, ListChanges, Mov, Op, Rem, SelectedListContent, UpdateItemUris,
    UriReplacement, op::Kind,
};
use protobuf::Message;

/// Both probe folder ids share this prefix, so cleanup can find them and the
/// guards can tell probe items from real ones. Spotify parses a group id as a
/// hexadecimal u64, so every character has to be a hex digit.
const PREFIX: &str = "fa57007ffee1d0";
const P: &str = "fa57007ffee1d00a";
const Q: &str = "fa57007ffee1d00b";

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

struct Rootlist {
    revision: Vec<u8>,
    uris: Vec<String>,
    /// The item count the server declares for the whole list, markers included.
    declared: i32,
    reads: usize,
}

impl Rootlist {
    fn index_of(&self, uri: &str) -> Option<usize> {
        self.uris.iter().position(|held| held == uri)
    }

    fn is_probe(&self, index: usize) -> bool {
        self.uris
            .get(index)
            .is_some_and(|uri| uri.to_ascii_lowercase().contains(PREFIX))
    }

    /// The tail from the first probe marker on: the only region any experiment
    /// is allowed to name, printed compactly.
    fn tail(&self) -> String {
        let Some(first) = self
            .uris
            .iter()
            .position(|uri| uri.to_ascii_lowercase().contains(PREFIX))
        else {
            return "(no probe items)".to_string();
        };
        self.uris[first..]
            .iter()
            .enumerate()
            .map(|(offset, uri)| {
                let label = match (
                    uri.strip_prefix("spotify:start-group:"),
                    uri.strip_prefix("spotify:end-group:"),
                ) {
                    (Some(rest), _) => format!("{}<", &rest[15..16]),
                    (_, Some(rest)) => format!("{}>", &rest[15..16]),
                    _ => "?".to_string(),
                };
                format!("{}:{label}", first + offset)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Reads the whole rootlist. The server honours a large `length`, so this is
/// normally one request; the loop is there for a library big enough to be
/// truncated anyway.
async fn read(session: &Session) -> Result<Rootlist> {
    const PAGE: usize = 2000;
    let mut uris: Vec<String> = Vec::new();
    let mut revision = Vec::new();
    let mut declared = 0;
    let mut reads = 0;
    loop {
        let bytes = session
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
        } else if page.revision() != revision.as_slice() {
            bail!("the revision changed while reading; start again");
        } else if page.length() != declared {
            bail!("the declared length changed while reading; start again");
        }
        let contents = page
            .contents
            .as_ref()
            .ok_or_else(|| anyhow!("rootlist page {reads} has no contents"))?;
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
    let kinds: std::collections::BTreeSet<_> = list
        .uris
        .iter()
        .map(|uri| uri.split(':').nth(1).unwrap_or("?"))
        .collect();
    println!("{folders} folders, depth back to {depth}, item kinds {kinds:?}");
}

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

fn add(at: usize, uris: &[String]) -> Op {
    let mut add = Add::new();
    add.set_from_index(at as i32);
    for uri in uris {
        add.items.push(item(uri));
    }
    let mut o = op(Kind::ADD);
    o.add = Some(add).into();
    o
}

fn rem(at: usize, uris: &[String]) -> Op {
    let mut rem = Rem::new();
    rem.set_from_index(at as i32);
    rem.set_length(uris.len() as i32);
    for uri in uris {
        rem.items.push(item(uri));
    }
    let mut o = op(Kind::REM);
    o.rem = Some(rem).into();
    o
}

fn mov(from: usize, length: usize, to: usize) -> Op {
    let mut mov = Mov::new();
    mov.set_from_index(from as i32);
    mov.set_length(length as i32);
    mov.set_to_index(to as i32);
    let mut o = op(Kind::MOV);
    o.mov = Some(mov).into();
    o
}

fn folder(id: &str, name: &str) -> [String; 2] {
    [start_uri(id, name), end_uri(id)]
}

/// One change, exactly as the application would send it. The operations of a
/// delta apply in order, so an op that shifts the list changes the indexes the
/// ops after it see.
async fn post(session: &Session, user: &str, base: &[u8], ops: Vec<Op>) -> String {
    let mut delta = Delta::new();
    delta.ops = ops;
    let mut changes = ListChanges::new();
    changes.set_base_revision(base.to_vec());
    changes.deltas.push(delta);
    let endpoint = format!("/playlist/v2/user/{user}/rootlist/changes");
    match post_once(session, &endpoint, &changes).await {
        Ok(_) => "accepted".to_string(),
        Err(error) => format!("rejected ({error})"),
    }
}

/// Sends a mutation once. `SpClient::request_with_protobuf` retries network
/// failures up to ten times, which is unsafe for an ADD when the first reply
/// was lost after Spotify applied it. The underlying HTTP client only repeats
/// an explicit 429 response, where Spotify rejected the request before asking
/// the client to wait.
async fn post_once(session: &Session, endpoint: &str, changes: &ListChanges) -> Result<()> {
    let body = changes.write_to_bytes()?;
    let base = session.spclient().base_url().await?;
    let url = format!(
        "{base}{endpoint}?product=0&country={}&salt={}",
        session.country(),
        rand::random::<u32>()
    );
    let token = session.login5().auth_token().await?;
    let mut request = http::Request::builder()
        .method(http::Method::POST)
        .uri(url)
        .header(http::header::CONTENT_TYPE, "application/x-protobuf")
        .header(http::header::CONTENT_LENGTH, body.len())
        .header(
            http::header::AUTHORIZATION,
            format!("{} {}", token.token_type, token.access_token),
        );
    if let Ok(client_token) = session.spclient().client_token().await {
        request = request.header("client-token", client_token);
    }
    let request = request.body(body.into())?;
    session.http_client().request_body(request).await?;
    Ok(())
}

/// Removes every folder this probe ever created, in balanced marker pairs.
async fn clean(session: &Session, user: &str) -> Result<()> {
    loop {
        let list = read(session).await?;
        // Group ids are case insensitive, so the search has to be too.
        let Some(start) = list.uris.iter().position(|uri| {
            uri.to_ascii_lowercase()
                .starts_with(&format!("spotify:start-group:{PREFIX}"))
        }) else {
            return Ok(());
        };
        let id = list.uris[start]["spotify:start-group:".len()..]
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string();
        let end = list
            .index_of(&end_uri(&id))
            .ok_or_else(|| anyhow!("folder {id} has no end marker"))?;
        if !list.is_probe(start) || !list.is_probe(end) {
            bail!("refusing to remove an index that is not a probe marker");
        }
        // The server refuses a delta that leaves the group markers unbalanced,
        // so both go in one request, the later index first.
        let outcome = post(
            session,
            user,
            &list.revision,
            vec![
                rem(end, &[end_uri(&id)]),
                rem(start, &[list.uris[start].clone()]),
            ],
        )
        .await;
        if outcome != "accepted" {
            bail!("cleanup {outcome}");
        }
        println!("  removed folder {id} at {start}..={end}");
    }
}

/// Leaves exactly two empty probe folders, adjacent, at the end of the list.
async fn reset(session: &Session, user: &str) -> Result<Rootlist> {
    clean(session, user).await?;
    let list = read(session).await?;
    let at = list.uris.len();
    let outcome = post(
        session,
        user,
        &list.revision,
        vec![
            add(at, &folder(P, "probe a")),
            add(at + 2, &folder(Q, "probe b")),
        ],
    )
    .await;
    if outcome != "accepted" {
        bail!("setup {outcome}");
    }
    read(session).await
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

    let baseline = read(&session).await?;

    if arg("--clean") {
        clean(&session, &user).await?;
        println!("clean");
        return Ok(());
    }
    if !arg("--write") {
        print_tree(&baseline);
        println!("\n(read only; pass --write for the experiments)");
        return Ok(());
    }

    let outcome = experiments(&session, &user).await;
    clean(&session, &user).await?;
    let after = read(&session).await?;
    println!(
        "\n=== library restored: {} ===",
        after.uris == baseline.uris
    );
    outcome
}

async fn experiments(session: &Session, user: &str) -> Result<()> {
    println!("\n### how much comes back in one request");
    for wanted in [100usize, 500, 5000] {
        match session.spclient().get_rootlist(0, Some(wanted)).await {
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

    println!("\n### which folder ids the server accepts");
    for candidate in [
        "fa57007ffee1d0cc",
        "FA57007FFEE1D0CD",
        "fa57007ffee1d0c",
        "fa57007ffee1d0ccc",
        "fa57007ffee1d0-x",
    ] {
        let list = read(session).await?;
        let outcome = post(
            session,
            user,
            &list.revision,
            vec![add(list.uris.len(), &folder(candidate, "id test"))],
        )
        .await;
        println!("  {candidate:<18} {outcome}");
        clean(session, user).await?;
    }

    println!("\n### unicode, spaces and punctuation in a folder name");
    let name = "probe ünïcode +plus %pct";
    let list = read(session).await?;
    post(
        session,
        user,
        &list.revision,
        vec![add(list.uris.len(), &folder(P, name))],
    )
    .await;
    let list = read(session).await?;
    match list.index_of(&start_uri(P, name)) {
        Some(index) => println!(
            "  round trip at {index}: {:?}",
            decode_name(&list.uris[index][format!("spotify:start-group:{P}:").len()..])
        ),
        None => println!("  the name did not survive: {}", list.tail()),
    }

    println!("\n### rename: UPDATE_ITEM_URIS on the start marker alone");
    let list = read(session).await?;
    let start = list
        .index_of(&start_uri(P, name))
        .ok_or_else(|| anyhow!("probe folder missing"))?;
    // A child, so the rename can be shown not to disturb the contents.
    post(
        session,
        user,
        &list.revision,
        vec![add(start + 1, &folder(Q, "child"))],
    )
    .await;
    let list = read(session).await?;
    let start = list.index_of(&start_uri(P, name)).unwrap();
    let mut replacement = UriReplacement::new();
    replacement.set_index(start as i32);
    replacement.item = Some(item(&start_uri(P, name))).into();
    replacement.set_new_uri(start_uri(P, "probe renamed"));
    let mut update = UpdateItemUris::new();
    update.uri_replacements.push(replacement);
    let mut o = op(Kind::UPDATE_ITEM_URIS);
    o.update_item_uris = Some(update).into();
    let outcome = post(session, user, &list.revision, vec![o]).await;
    let list = read(session).await?;
    let renamed = list.index_of(&start_uri(P, "probe renamed"));
    println!(
        "  {outcome}; renamed {}, child still inside {}",
        renamed.is_some(),
        list.index_of(&start_uri(Q, "child")) == renamed.map(|index| index + 1),
    );

    println!("\n### does the server read Op.items, or only the indexes?");
    let list = reset(session, user).await?;
    println!("  layout {}", list.tail());
    let q_start = list.index_of(&start_uri(Q, "probe b")).unwrap();
    let outcome = post(
        session,
        user,
        &list.revision,
        vec![rem(q_start, &folder(P, "probe a"))],
    )
    .await;
    let after = read(session).await?;
    println!(
        "  REM at b's index carrying a's items: {outcome}; a present {}, b present {}",
        after.index_of(&start_uri(P, "probe a")).is_some(),
        after.index_of(&start_uri(Q, "probe b")).is_some(),
    );

    let list = reset(session, user).await?;
    let p_start = list.index_of(&start_uri(P, "probe a")).unwrap();
    let mut bare = Rem::new();
    bare.set_from_index(p_start as i32);
    bare.set_length(2);
    let mut o = op(Kind::REM);
    o.rem = Some(bare).into();
    let outcome = post(session, user, &list.revision, vec![o]).await;
    let after = read(session).await?;
    println!(
        "  REM by index with no items at all: {outcome}; a present {}",
        after.index_of(&start_uri(P, "probe a")).is_some(),
    );

    println!("\n### does MOV read its items, or only from_index?");
    let list = reset(session, user).await?;
    println!("  layout {}", list.tail());
    let p_start = list.index_of(&start_uri(P, "probe a")).unwrap();
    let q_start = list.index_of(&start_uri(Q, "probe b")).unwrap();
    // Move b to the front while carrying a's items. If the items decided, a
    // would move to the index it already holds and nothing would change.
    let mut m = Mov::new();
    m.set_from_index(q_start as i32);
    m.set_length(2);
    m.set_to_index(p_start as i32);
    m.items.push(item(&start_uri(P, "probe a")));
    m.items.push(item(&end_uri(P)));
    let mut o = op(Kind::MOV);
    o.mov = Some(m).into();
    let outcome = post(session, user, &list.revision, vec![o]).await;
    let after = read(session).await?;
    println!(
        "  MOV of b's span carrying a's items: {outcome}; {}   b now at {:?}",
        after.tail(),
        after.index_of(&start_uri(Q, "probe b")),
    );

    println!("\n### does UPDATE_ITEM_URIS need its item?");
    let list = reset(session, user).await?;
    let p_start = list.index_of(&start_uri(P, "probe a")).unwrap();
    // The index alone, with no item travelling beside it.
    let mut replacement = UriReplacement::new();
    replacement.set_index(p_start as i32);
    replacement.set_new_uri(start_uri(P, "renamed by index alone"));
    let mut update = UpdateItemUris::new();
    update.uri_replacements.push(replacement);
    let mut o = op(Kind::UPDATE_ITEM_URIS);
    o.update_item_uris = Some(update).into();
    let outcome = post(session, user, &list.revision, vec![o]).await;
    let after = read(session).await?;
    println!(
        "  index alone, no item: {outcome}; a renamed {}",
        after
            .index_of(&start_uri(P, "renamed by index alone"))
            .is_some(),
    );

    // The right item at the wrong index. If the item won, a would be renamed.
    let list = reset(session, user).await?;
    let q_start = list.index_of(&start_uri(Q, "probe b")).unwrap();
    let mut replacement = UriReplacement::new();
    replacement.set_index(q_start as i32);
    replacement.item = Some(item(&start_uri(P, "probe a"))).into();
    replacement.set_new_uri(start_uri(P, "renamed by item"));
    let mut update = UpdateItemUris::new();
    update.uri_replacements.push(replacement);
    let mut o = op(Kind::UPDATE_ITEM_URIS);
    o.update_item_uris = Some(update).into();
    let outcome = post(session, user, &list.revision, vec![o]).await;
    let after = read(session).await?;
    println!(
        "  a's item at b's index: {outcome}; a renamed {}, b untouched {}",
        after.index_of(&start_uri(P, "renamed by item")).is_some(),
        after.index_of(&start_uri(Q, "probe b")).is_some(),
    );

    // And a mismatch the other way round: the right index, the wrong item.
    let list = reset(session, user).await?;
    let p_start = list.index_of(&start_uri(P, "probe a")).unwrap();
    let mut replacement = UriReplacement::new();
    replacement.set_index(p_start as i32);
    replacement.item = Some(item(&start_uri(Q, "probe b"))).into();
    replacement.set_new_uri(start_uri(P, "renamed by neither"));
    let mut update = UpdateItemUris::new();
    update.uri_replacements.push(replacement);
    let mut o = op(Kind::UPDATE_ITEM_URIS);
    o.update_item_uris = Some(update).into();
    let outcome = post(session, user, &list.revision, vec![o]).await;
    let after = read(session).await?;
    println!(
        "  b's item at a's index: {outcome}; a renamed {}",
        after
            .index_of(&start_uri(P, "renamed by neither"))
            .is_some(),
    );

    println!("\n### can one marker be removed on its own?");
    let list = reset(session, user).await?;
    let p_start = list.index_of(&start_uri(P, "probe a")).unwrap();
    let outcome = post(
        session,
        user,
        &list.revision,
        vec![rem(p_start, &[start_uri(P, "probe a")])],
    )
    .await;
    println!("  REM of the start marker alone: {outcome}");

    println!("\n### how Mov.to_index is counted");
    for offset in [0i32, 1, -1] {
        let list = reset(session, user).await?;
        let from = list.index_of(&start_uri(P, "probe a")).unwrap();
        let q_end = list.index_of(&end_uri(Q)).unwrap();
        let to = (q_end as i32 + offset) as usize;
        let outcome = post(session, user, &list.revision, vec![mov(from, 2, to)]).await;
        let after = read(session).await?;
        println!(
            "  move a (at {from}, length 2) to_index {to}: {outcome}; {}",
            after.tail()
        );
    }

    println!("\n### base_revision");
    let list = reset(session, user).await?;
    let p_start = list.index_of(&start_uri(P, "probe a")).unwrap();
    let garbage = vec![0u8; list.revision.len()];
    println!(
        "  a revision the server never issued: {}",
        post(
            session,
            user,
            &garbage,
            vec![rem(p_start, &folder(P, "probe a"))]
        )
        .await
    );

    let list = reset(session, user).await?;
    let stale = list.revision.clone();
    let p_start = list.index_of(&start_uri(P, "probe a")).unwrap();
    let q_start = list.index_of(&start_uri(Q, "probe b")).unwrap();
    println!(
        "  before {}   (a at {p_start}, b at {q_start})",
        list.tail()
    );
    // Swap the two folders, so the index a had at the stale revision now holds b.
    post(
        session,
        user,
        &list.revision,
        vec![mov(q_start, 2, p_start)],
    )
    .await;
    let swapped = read(session).await?;
    println!(
        "  after a change from elsewhere {}   (a at {:?}, b at {:?})",
        swapped.tail(),
        swapped.index_of(&start_uri(P, "probe a")),
        swapped.index_of(&start_uri(Q, "probe b")),
    );
    if !swapped.is_probe(p_start) || !swapped.is_probe(p_start + 1) {
        println!("  (skipped: the stale index left probe territory)");
        return Ok(());
    }
    let outcome = post(
        session,
        user,
        &stale,
        vec![rem(p_start, &folder(P, "probe a"))],
    )
    .await;
    let after = read(session).await?;
    let a_gone = after.index_of(&start_uri(P, "probe a")).is_none();
    println!(
        "  replaying a delta built at the older revision: {outcome}; a present {}, b present {}",
        !a_gone,
        after.index_of(&start_uri(Q, "probe b")).is_some(),
    );
    println!(
        "  -> the server {}",
        if a_gone {
            "REBASED the stale delta onto the change it had not seen"
        } else {
            "applied the stale index LITERALLY and hit the wrong folder"
        }
    );

    let list = reset(session, user).await?;
    let stale = list.revision.clone();
    let p_old = list.index_of(&start_uri(P, "probe a")).unwrap();
    let q_old = list.index_of(&start_uri(Q, "probe b")).unwrap();
    post(session, user, &list.revision, vec![mov(q_old, 2, p_old)]).await;
    let swapped = read(session).await?;
    let p_fresh = swapped.index_of(&start_uri(P, "probe a")).unwrap();
    let outcome = post(
        session,
        user,
        &stale,
        vec![rem(p_fresh, &folder(P, "probe a"))],
    )
    .await;
    let after = read(session).await?;
    println!(
        "  fresh index {p_fresh} with the older revision: {outcome}; a present {}, b present {}",
        after.index_of(&start_uri(P, "probe a")).is_some(),
        after.index_of(&start_uri(Q, "probe b")).is_some(),
    );
    println!(
        "  -> the server rebased the index from the claimed snapshot, so mixing snapshots hit {}",
        if after.index_of(&start_uri(Q, "probe b")).is_none() {
            "the wrong folder"
        } else {
            "an unexpected target"
        }
    );
    Ok(())
}
