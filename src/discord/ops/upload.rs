//! Getting a file to Discord.
//!
//! There are two ways, and this module uses the newer one and keeps the older
//! as a fallback, because the difference between them matters on a slow link.
//!
//! **The three-request way.** `POST /channels/{id}/attachments` asks for a slot
//! and comes back with a signed URL on Google's storage; the bytes go there
//! with a `PUT`; the message is then posted with nothing in it but the name of
//! the slot. The bytes never touch Discord's API, so a 429 on the message route
//! costs one small retry rather than twenty megabytes again, and the upload
//! itself is not counted against any allowance at all.
//!
//! **The one-request way.** The bytes go inside the message as a multipart
//! form. This is what Discord's clients did before the slot endpoint existed,
//! and it is what happens here when the slot endpoint answers 403 or 404 — on
//! some channels and some accounts it is simply not available. There is no
//! fallback *from* the fallback: a failure there is reported.
//!
//! **The cap is checked before anything is sent.** `[media] max_attachment_mib`
//! is refused locally rather than by asking Discord to refuse it, because a
//! rejected request is still a request and, on a user account, a line in
//! somebody's ledger. The default is 25 MiB, which is what an account without
//! Nitro is allowed.
//!
//! The URL that comes back from the slot request is on a host Discord does not
//! control, so the `PUT` goes out on the media client, which has no path by
//! which a token could reach it. That is not a detail; it is the reason the two
//! clients are separate at all.

use std::path::Path;
use std::sync::Arc;

use crate::discord::handle::{Event, Nonce, Upload};
use crate::discord::http::api::{
    self, AttachmentRef, AttachmentSlotRequest, AttachmentSlots, MultipartFile,
};
use crate::discord::snowflake::ChannelId;

use super::Ops;

/// What a file is called when nothing says otherwise.
const UNNAMED: &str = "file";

/// A file, read and ready to go, whichever way it ends up going.
#[derive(Clone)]
pub struct Prepared {
    pub filename: String,
    pub content_type: String,
    pub bytes: Arc<Vec<u8>>,
}

/// `Debug` by hand: the derived one would print every byte of a picture.
impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Prepared({}, {}, {} bytes)",
            self.filename,
            self.content_type,
            self.bytes.len()
        )
    }
}

impl Prepared {
    fn slot_request(&self, index: usize) -> AttachmentSlotRequest<'_> {
        AttachmentSlotRequest {
            filename: &self.filename,
            file_size: self.bytes.len() as u64,
            id: index.to_string(),
        }
    }

    fn multipart(&self) -> MultipartFile {
        MultipartFile {
            filename: self.filename.clone(),
            content_type: self.content_type.clone(),
            bytes: Arc::clone(&self.bytes),
        }
    }
}

/// A filename that is a filename and not a path.
///
/// The name goes into a URL and into somebody else's file listing, so anything
/// with a separator in it is taken apart and only the last component kept. A
/// name that is nothing but separators becomes `file`.
fn safe_name(raw: &str) -> String {
    let name = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(UNNAMED)
        .trim()
        .trim_matches('.');
    if name.is_empty() {
        UNNAMED.to_string()
    } else {
        name.to_string()
    }
}

/// What the bytes are, from the name.
///
/// The name is the only evidence there is at this point: nothing has decoded
/// the file and nothing is going to. `application/octet-stream` is the honest
/// answer when the extension says nothing, and Discord accepts it.
pub fn content_type_for(filename: &str) -> String {
    mime_guess::from_path(filename)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// Read what was asked for and check it against the cap.
///
/// Everything here happens before a single request: the read, the naming and
/// the size check. A file too large to send is refused with a sentence rather
/// than by watching Discord refuse it.
pub async fn prepare(uploads: &[Upload], cap: u64) -> Result<Vec<Prepared>, String> {
    let mut out = Vec::with_capacity(uploads.len());
    let mut total = 0u64;

    for upload in uploads {
        let prepared = match upload {
            Upload::Path(path) => read_path(path).await?,
            Upload::Bytes {
                filename,
                data,
                content_type,
            } => {
                let filename = safe_name(filename);
                let content_type = if content_type.is_empty() {
                    content_type_for(&filename)
                } else {
                    content_type.clone()
                };
                Prepared {
                    filename,
                    content_type,
                    bytes: Arc::clone(data),
                }
            }
        };

        let size = prepared.bytes.len() as u64;
        if size > cap {
            return Err(format!(
                "{} is {} and the limit is {}",
                prepared.filename,
                mib(size),
                mib(cap)
            ));
        }
        total += size;
        if total > cap {
            return Err(format!(
                "those files come to {} and the limit is {} for one message",
                mib(total),
                mib(cap)
            ));
        }
        out.push(prepared);
    }

    Ok(out)
}

async fn read_path(path: &Path) -> Result<Prepared, String> {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(safe_name)
        .unwrap_or_else(|| UNNAMED.to_string());

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;

    Ok(Prepared {
        content_type: content_type_for(&filename),
        filename,
        bytes: Arc::new(bytes),
    })
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// What staging the files produced.
#[derive(Debug)]
pub enum Staged {
    /// The bytes are up. Send the message with these references.
    Uploaded(Vec<AttachmentRef>),
    /// The slot endpoint refused. Put the bytes in the message itself.
    SendInline,
}

/// Get the bytes to Discord, however it will take them.
///
/// Emits [`Event::UploadProgress`] as the bytes leave, which is the only thing
/// that distinguishes a slow upload from a wedged one on screen.
pub async fn stage(
    ops: &Ops,
    channel: ChannelId,
    nonce: Nonce,
    files: &[Prepared],
) -> Result<Staged, String> {
    if files.is_empty() {
        return Ok(Staged::Uploaded(Vec::new()));
    }

    let requests: Vec<AttachmentSlotRequest<'_>> = files
        .iter()
        .enumerate()
        .map(|(index, file)| file.slot_request(index))
        .collect();

    let slots: AttachmentSlots = match ops
        .rest(api::create_attachments(&ops.http, channel, &requests))
        .await
    {
        Ok(slots) => slots,
        Err(e) if e.is_gone() => {
            // Not an error: some channels and some accounts do not have this
            // endpoint, and the answer is to send the file the older way.
            tracing::debug!("no attachment slots for {channel} ({e}); sending inline");
            return Ok(Staged::SendInline);
        }
        Err(e) => return Err(e.to_string()),
    };

    if slots.attachments.len() < files.len() {
        tracing::debug!(
            "asked for {} slots and got {}; sending inline",
            files.len(),
            slots.attachments.len()
        );
        return Ok(Staged::SendInline);
    }

    let total: u64 = files.iter().map(|f| f.bytes.len() as u64).sum();
    let mut done = 0u64;
    let mut refs = Vec::with_capacity(files.len());

    for (index, file) in files.iter().enumerate() {
        // Matched by the index that was asked for rather than by position: the
        // slots come back in whatever order the server felt like.
        let slot = slots
            .attachments
            .iter()
            .find(|s| s.id as usize == index)
            .ok_or_else(|| format!("discord gave no slot for {}", file.filename))?;

        let events = ops.bridge.events.clone();
        let before = done;
        ops.http
            .put_bytes(
                &slot.upload_url,
                Arc::clone(&file.bytes),
                &file.content_type,
                move |sent, _| {
                    events.send(Event::UploadProgress {
                        nonce,
                        sent: before + sent,
                        total,
                    });
                },
            )
            .await
            .map_err(|e| format!("{} did not upload: {e}", file.filename))?;

        done += file.bytes.len() as u64;
        refs.push(AttachmentRef {
            id: index.to_string(),
            filename: file.filename.clone(),
            uploaded_filename: slot.upload_filename.clone(),
        });
    }

    Ok(Staged::Uploaded(refs))
}

/// The files as multipart parts, for the fallback path.
pub fn inline(files: &[Prepared]) -> Vec<MultipartFile> {
    files.iter().map(Prepared::multipart).collect()
}

/// What the message body should say about files sent inline.
///
/// The names still go in the JSON, without an `uploaded_filename`, because that
/// is what pairs `files[0]` with the entry describing it.
pub fn inline_refs(files: &[Prepared]) -> Vec<AttachmentRef> {
    files
        .iter()
        .enumerate()
        .map(|(index, file)| AttachmentRef {
            id: index.to_string(),
            filename: file.filename.clone(),
            uploaded_filename: String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(name: &str, len: usize) -> Upload {
        Upload::Bytes {
            filename: name.into(),
            data: Arc::new(vec![0u8; len]),
            content_type: String::new(),
        }
    }

    #[test]
    fn a_name_with_a_path_in_it_keeps_only_the_last_part() {
        assert_eq!(safe_name("/home/sam/holiday.png"), "holiday.png");
        assert_eq!(safe_name(r"C:\Users\sam\a.png"), "a.png");
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("   "), "file");
        assert_eq!(safe_name("..."), "file");
        assert_eq!(safe_name("plain.txt"), "plain.txt");
    }

    #[test]
    fn the_type_is_guessed_from_the_name_and_falls_back_to_bytes() {
        assert_eq!(content_type_for("a.png"), "image/png");
        assert_eq!(content_type_for("a.gif"), "image/gif");
        assert_eq!(content_type_for("a.webm"), "video/webm");
        assert_eq!(
            content_type_for("no-extension"),
            "application/octet-stream",
            "an honest answer beats a guessed one"
        );
    }

    #[tokio::test]
    async fn a_pasted_image_is_prepared_without_touching_a_disk() {
        let upload = Upload::Bytes {
            filename: "pasted.png".into(),
            data: Arc::new(vec![1, 2, 3]),
            content_type: "image/png".into(),
        };
        let prepared = prepare(&[upload], 1024).await.unwrap();
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].filename, "pasted.png");
        assert_eq!(prepared[0].content_type, "image/png");
        assert_eq!(prepared[0].bytes.len(), 3);
        assert!(
            format!("{:?}", prepared[0]).contains("3 bytes"),
            "Debug must not print the picture"
        );
    }

    #[tokio::test]
    async fn a_file_over_the_cap_is_refused_here_rather_than_by_discord() {
        let refused = prepare(&[bytes("big.png", 2048)], 1024).await.unwrap_err();
        assert!(refused.contains("big.png"), "{refused}");
        assert!(refused.contains("limit"), "{refused}");
    }

    /// Three files each under the cap can still be over it together, and
    /// Discord counts the message rather than the file.
    #[tokio::test]
    async fn files_that_are_only_too_large_together_are_refused_too() {
        let refused = prepare(&[bytes("a.png", 600), bytes("b.png", 600)], 1024)
            .await
            .unwrap_err();
        assert!(refused.contains("one message"), "{refused}");
    }

    #[tokio::test]
    async fn a_file_that_is_not_there_is_a_sentence_rather_than_a_panic() {
        let missing = Upload::Path("/definitely/not/here.png".into());
        let refused = prepare(&[missing], 1024).await.unwrap_err();
        assert!(refused.contains("here.png"), "{refused}");
    }

    #[tokio::test]
    async fn a_file_on_disk_is_read_and_named_from_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cat.png");
        std::fs::write(&path, b"not really a png").unwrap();

        let prepared = prepare(&[Upload::Path(path)], 1024).await.unwrap();
        assert_eq!(prepared[0].filename, "cat.png");
        assert_eq!(prepared[0].content_type, "image/png");
        assert_eq!(prepared[0].bytes.len(), 16);
    }

    // -- the whole flow, against a server ---------------------------------

    use crate::discord::handle::MessagesChange;
    use crate::discord::ops::send::send_message;
    use crate::discord::snowflake::ChannelId;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CHANNEL: ChannelId = ChannelId(7);

    fn posted_message() -> serde_json::Value {
        serde_json::json!({
            "id": "500",
            "channel_id": "7",
            "content": "look",
            "author": {"id": "1", "username": "sam"}
        })
    }

    fn a_png() -> Upload {
        Upload::Bytes {
            filename: "cat.png".into(),
            data: Arc::new(vec![0x89, b'P', b'N', b'G', 0, 0, 0, 0]),
            content_type: "image/png".into(),
        }
    }

    /// The ordinary path: ask for a slot, put the bytes somewhere that is not
    /// Discord, then post a message that names the slot.
    #[tokio::test]
    async fn a_file_goes_to_a_slot_and_the_message_only_names_it() {
        let server = MockServer::start().await;
        let upload_url = format!("{}/upload/abc", server.uri());

        Mock::given(method("POST"))
            .and(path("/channels/7/attachments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "attachments": [{
                    "id": 0,
                    "upload_url": upload_url,
                    "upload_filename": "9999/cat.png"
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/upload/abc"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/channels/7/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(posted_message()))
            .mount(&server)
            .await;

        let h = super::super::testing::harness(&server.uri());
        h.ops.state_mut().messages_mut(CHANNEL).set_at_latest(true);
        send_message(&h.ops, CHANNEL, "look".into(), None, false, vec![a_png()]).await;

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3, "slot, bytes, message");

        let asked: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(asked["files"][0]["filename"], "cat.png");
        assert_eq!(asked["files"][0]["file_size"], 8);
        assert_eq!(
            asked["files"][0]["id"], "0",
            "the index is a string, which discord is particular about"
        );

        assert_eq!(requests[1].body, vec![0x89, b'P', b'N', b'G', 0, 0, 0, 0]);
        assert_eq!(
            requests[1]
                .headers
                .get("content-type")
                .map(|v| v.to_str().unwrap()),
            Some("image/png")
        );
        assert!(
            requests[1].headers.get("authorization").is_none(),
            "the upload host is not discord and must never see a token"
        );

        let posted: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
        assert_eq!(posted["content"], "look");
        assert_eq!(posted["attachments"][0]["id"], "0");
        assert_eq!(posted["attachments"][0]["filename"], "cat.png");
        assert_eq!(
            posted["attachments"][0]["uploaded_filename"],
            "9999/cat.png"
        );

        let events: Vec<Event> = h.events.try_iter().collect();
        let progress: Vec<(u64, u64)> = events
            .iter()
            .filter_map(|e| match e {
                Event::UploadProgress { sent, total, .. } => Some((*sent, *total)),
                _ => None,
            })
            .collect();
        assert_eq!(progress, vec![(8, 8)], "nothing said the bytes were moving");
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::SendResult { result: Ok(_), .. })));
    }

    /// The slot endpoint is not available everywhere. A 403 there is not a
    /// failure; it means sending the file the older way.
    #[tokio::test]
    async fn a_refused_slot_sends_the_bytes_inside_the_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/channels/7/attachments"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "code": 50001, "message": "Missing Access"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/channels/7/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(posted_message()))
            .mount(&server)
            .await;

        let h = super::super::testing::harness(&server.uri());
        h.ops.state_mut().messages_mut(CHANNEL).set_at_latest(true);
        send_message(&h.ops, CHANNEL, "look".into(), None, false, vec![a_png()]).await;

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "the slot request, then the message");

        let content_type = requests[1]
            .headers
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        assert!(
            content_type.starts_with("multipart/form-data"),
            "{content_type}"
        );

        let body = String::from_utf8_lossy(&requests[1].body);
        assert!(body.contains("payload_json"), "{body}");
        assert!(body.contains("files[0]"), "{body}");
        assert!(body.contains("cat.png"), "{body}");
        assert!(
            body.contains("\"uploaded_filename\":\"\""),
            "an inline file names no slot: {body}"
        );

        assert!(h
            .events
            .try_iter()
            .any(|e| matches!(e, Event::SendResult { result: Ok(_), .. })));
    }

    /// The cap is this client's, checked here. A refused request is still a
    /// request, and the size is knowable without making one.
    #[tokio::test]
    async fn a_file_over_the_cap_never_reaches_the_network() {
        let server = MockServer::start().await;
        // Nothing is mounted: any request at all fails the test.

        let h = super::super::testing::harness(&server.uri());
        let huge = Upload::Bytes {
            filename: "huge.bin".into(),
            data: Arc::new(vec![0u8; 2 * 1024 * 1024]),
            content_type: String::new(),
        };
        // One mebibyte, rather than the 25 the default allows.
        let ops = Ops {
            media: Arc::new(crate::discord::media::MediaConfig {
                max_attachment_mib: 1,
                ..Default::default()
            }),
            ..h.ops.clone()
        };
        send_message(&ops, CHANNEL, "look".into(), None, false, vec![huge]).await;

        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "a file this client refuses must not be sent for discord to refuse"
        );

        let events: Vec<Event> = h.events.try_iter().collect();
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::SendResult {
                    result: Err(reason),
                    ..
                } if reason.contains("huge.bin")
            )),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Messages(_, MessagesChange::Pending(_)))),
            "there should be no optimistic row for something never sent"
        );
    }

    #[test]
    fn an_inline_reference_names_the_file_and_no_slot() {
        let files = vec![Prepared {
            filename: "a.png".into(),
            content_type: "image/png".into(),
            bytes: Arc::new(vec![0; 4]),
        }];
        let refs = inline_refs(&files);
        assert_eq!(refs[0].id, "0", "discord wants the index as a string");
        assert_eq!(refs[0].filename, "a.png");
        assert!(refs[0].uploaded_filename.is_empty());
    }
}
